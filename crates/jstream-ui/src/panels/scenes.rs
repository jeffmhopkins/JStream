//! The Scenes panel (spec-panels.md 1): the collection's scenes in display order, under
//! a search box and a favourites and labels filter, each row with its icon, a "program"
//! badge on the scene on program, the chips of its labels (ADR-0078), the tag button
//! whose popover opens under the row, and a favourite star. The label manager opens
//! over the rows when "Edit labels…" is chosen here (`labels.rs`). Clicking a scene shows it in
//! Sources and the canvas and puts it on program (`scene.switch`), or, in studio mode,
//! on preview (`studio.set_preview`), where a hover button puts it on program; a mouse
//! picks on the press, as OBS does, and a touch, a pen or the keyboard on the click. A double
//! click opens its Properties dialog (a keyframe's renames it). Rows are reordered by
//! dragging them (`scene.reorder`), renamed in place (F2) and deleted (Del or the menu)
//! through `delete.plan`, so the user sees what else goes. A right-click on a scene
//! opens its menu, and one on the panel's empty space opens the panel's
//! (spec-workspace.md 5.2.3 and 5.2.7). The tool bar under the rows (SC-11) adds a
//! scene beside the selected one, deletes it, opens its Properties or Filters, and moves
//! it one place up or down among the scenes of its folder.
//!
//! A scene's keyframe scenes (ADR-0024 §2) nest under it, each with the purple diamond,
//! then its sequences, and the scene's caret folds them away. The scene's own folders
//! (ADR-0219) come first under it and hold its keyframes and sequences, for organising
//! only: the scene menu's "New folder under" makes one, and a keyframe, a sequence or
//! one of those folders is dragged, or moved with its menu's "Move to folder", only
//! among the rows under its own scene. A keyframe's menu switches to it, resets it
//! to its parent (`item.clear_override` for every field it overrides), makes another
//! keyframe of the parent (`scene.create_variant`) and deletes it, at once, since it
//! holds no items of its own (undo brings it back).
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#scenes

use crate::app::SessionContext;
use crate::dock::PanelId;
use crate::icons::{Icon, named};
use crate::menu::{Entry, Menus};
use crate::panels::confirm::{Confirm, Pending};
use crate::panels::filters::{FilterDialogs, show_filters};
use crate::panels::labels::{
    LabelChips, LabelEditor, LabelManager, LabelPopover, TagButton, TagPopover, Tagged, carried,
};
use crate::panels::scene_properties::SceneDialog;
use crate::panels::sequence;
use crate::panels::studio::override_submenu;
use crate::panels::tree::{
    Filter, Reorder, SearchBar, Side, Star, Tool, ToolBar, dropped_at, indent,
};
use crate::store::{Collection, Studio, apply, plan_delete, plan_summary, reset_edits};
use crate::workspace::DockState;
use jstream_control::protocol::{
    Action, Chord, DeleteTarget, Edit, FilterOwner, FolderId, KeyframeEdge, LabelId, SceneEdge,
    SceneId, SceneInfo, TimelineId, TimelineInfo, TimelineTrigger,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::HashSet;

/// The prefix that keeps a folder's row key apart from a scene's.
const FOLDER_KEY: &str = "folder:";

/// The prefix of a sequence row's key, for renaming it.
const SEQUENCE_KEY: &str = "sequence:";

/// One row of the tree, as the view needs it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Row {
    /// A folder (S13): `open` when its contents show, `kept` when a filter is on and it
    /// shows only because something in it matches.
    Folder {
        folder_id: FolderId,
        name: String,
        depth: usize,
        open: bool,
        kept: bool,
    },
    /// A listed scene.
    Scene {
        scene_id: SceneId,
        name: String,
        /// The scene's icon, the keyframe diamond for a keyframe scene, or the scene icon.
        icon: &'static str,
        favourite: bool,
        depth: usize,
        /// For a keyframe scene, its parent and the parent's name.
        keyframe_of: Option<(SceneId, String)>,
        /// Whether the scene has keyframe scenes or sequences, so its row has a caret.
        keyframes: bool,
    },
    /// A scene's sequence (ADR-0184), after its keyframes.
    Sequence {
        timeline_id: TimelineId,
        name: String,
        depth: usize,
    },
}

impl Row {
    fn scene(
        scene: &SceneInfo,
        depth: usize,
        keyframe_of: Option<&SceneInfo>,
        keyframes: bool,
    ) -> Row {
        Row::Scene {
            scene_id: scene.scene_id.clone(),
            name: scene.name.clone(),
            icon: match keyframe_of {
                Some(_) => "keyframe",
                None => scene
                    .meta
                    .icon
                    .as_deref()
                    .map_or("layers", |icon| named(icon, "layers")),
            },
            favourite: scene.meta.favourite,
            depth,
            keyframe_of: keyframe_of.map(|p| (p.scene_id.clone(), p.name.clone())),
            keyframes,
        }
    }
}

/// What a row key names.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    Folder(FolderId),
    Scene(SceneId),
    Sequence(TimelineId),
}

impl Node {
    fn of(key: &str) -> Node {
        if let Some(id) = key.strip_prefix(FOLDER_KEY) {
            return Node::Folder(FolderId::new(id));
        }
        match key.strip_prefix(SEQUENCE_KEY) {
            Some(id) => Node::Sequence(TimelineId::new(id)),
            None => Node::Scene(SceneId::new(key)),
        }
    }
}

/// The rows a row moves among (ADR-0219): `Some(None)` for the scene list, or
/// `Some(Some(scene))` for what sits under `scene`: its folders, keyframes and
/// sequences. `None` for a row naming nothing.
fn space_of(collection: &Collection, node: &Node) -> Option<Option<SceneId>> {
    match node {
        Node::Scene(id) => Some(collection.scene(id)?.variant_of.clone()),
        Node::Folder(id) => Some(collection.folder(id)?.scene_id.clone()),
        Node::Sequence(id) => {
            let (scene, _) = collection.timeline(id)?;
            Some(Some(
                scene
                    .variant_of
                    .clone()
                    .unwrap_or_else(|| scene.scene_id.clone()),
            ))
        }
    }
}

/// `folder`, if it is one of `owner`'s folders (ADR-0219), or of the scene list for
/// `None`: anything filed in a folder that isn't shows unfiled.
fn filed_in(
    collection: &Collection,
    folder: Option<&FolderId>,
    owner: Option<&SceneId>,
) -> Option<FolderId> {
    folder
        .filter(|f| {
            collection
                .folder(f)
                .is_some_and(|f| f.scene_id.as_ref() == owner)
        })
        .cloned()
}

/// The folder a sequence is filed in under its scene, if it names one of its scene's.
fn sequence_folder(collection: &Collection, timeline_id: &TimelineId) -> Option<FolderId> {
    let (scene, timeline) = collection.timeline(timeline_id)?;
    let owner = scene.variant_of.as_ref().unwrap_or(&scene.scene_id);
    filed_in(collection, timeline.folder.as_ref(), Some(owner))
}

/// `owner`'s folders (ADR-0219) in tree order, each with its path of names, for a
/// "Move to folder" menu.
fn scene_folders(collection: &Collection, owner: &SceneId) -> Vec<(FolderId, String)> {
    fn walk(
        collection: &Collection,
        owner: &SceneId,
        parent: Option<&FolderId>,
        path: &str,
        seen: &mut HashSet<FolderId>,
        out: &mut Vec<(FolderId, String)>,
    ) {
        for folder in &collection.folders {
            if folder.scene_id.as_ref() != Some(owner)
                || parent_of(collection, &folder.folder_id).as_ref() != parent
                || !seen.insert(folder.folder_id.clone())
            {
                continue;
            }
            let path = if path.is_empty() {
                folder.name.clone()
            } else {
                format!("{path} / {}", folder.name)
            };
            out.push((folder.folder_id.clone(), path.clone()));
            walk(collection, owner, Some(&folder.folder_id), &path, seen, out);
        }
    }
    let mut out = Vec::new();
    walk(collection, owner, None, "", &mut HashSet::new(), &mut out);
    out
}

/// The folder `folder_id` sits in, if it names one that exists.
fn parent_of(collection: &Collection, folder_id: &FolderId) -> Option<FolderId> {
    collection
        .folder(folder_id)?
        .parent
        .clone()
        .filter(|p| collection.folder(p).is_some())
}

/// The folder a scene sits in, if it names one that exists and is of its kind: one of
/// its parent's for a keyframe scene (ADR-0219), else one of the scene list's.
fn folder_of(collection: &Collection, scene_id: &SceneId) -> Option<FolderId> {
    let scene = collection.scene(scene_id)?;
    filed_in(collection, scene.folder.as_ref(), scene.variant_of.as_ref())
}

/// Whether `folder_id` is `inside` or a folder somewhere within it.
fn within(collection: &Collection, folder_id: &FolderId, inside: &FolderId) -> bool {
    let mut at = Some(folder_id.clone());
    let mut seen = HashSet::new();
    while let Some(folder) = at {
        if &folder == inside {
            return true;
        }
        if !seen.insert(folder.clone()) {
            return false;
        }
        at = parent_of(collection, &folder);
    }
    false
}

/// The rows (spec-panels.md S5, S9, S10): each level's folders in folder order, then its
/// listed scenes in display order (groups are hidden), a folder's contents under it
/// unless it is `collapsed`. Each scene is followed, one level in, by its keyframe
/// scenes in display order unless it is `folded`; a keyframe scene whose parent isn't
/// listed shows on its own. With a filter on, folders never match on their own: one
/// shows, open and `kept`, only when something in it matches `shows`.
fn rows(
    collection: &Collection,
    collapsed: &HashSet<FolderId>,
    folded: &HashSet<SceneId>,
    filtering: bool,
    shows: &dyn Fn(&str, bool, &[LabelId]) -> bool,
) -> Vec<Row> {
    let mut walk = Walk {
        collection,
        collapsed,
        folded,
        filtering,
        shows,
        seen: HashSet::new(),
    };
    let mut out = Vec::new();
    walk.level(None, 0, &mut out);
    out
}

/// The state of [`rows`]' walk down the folders.
struct Walk<'a> {
    collection: &'a Collection,
    collapsed: &'a HashSet<FolderId>,
    folded: &'a HashSet<SceneId>,
    filtering: bool,
    shows: &'a dyn Fn(&str, bool, &[LabelId]) -> bool,
    /// The folders already walked, so a broken parent chain can't loop.
    seen: HashSet<FolderId>,
}

impl Walk<'_> {
    /// What sits in `parent` (the top level for `None`), at `depth`.
    fn level(&mut self, parent: Option<&FolderId>, depth: usize, out: &mut Vec<Row>) {
        let collection = self.collection;
        for folder in &collection.folders {
            if folder.scene_id.is_some()
                || parent_of(collection, &folder.folder_id).as_ref() != parent
                || !self.seen.insert(folder.folder_id.clone())
            {
                continue;
            }
            let mut inner = Vec::new();
            self.level(Some(&folder.folder_id), depth + 1, &mut inner);
            if self.filtering && inner.is_empty() {
                continue;
            }
            let open = self.filtering || !self.collapsed.contains(&folder.folder_id);
            out.push(Row::Folder {
                folder_id: folder.folder_id.clone(),
                name: folder.name.clone(),
                depth,
                open,
                kept: self.filtering,
            });
            if open {
                out.extend(inner);
            }
        }
        let listed: Vec<&SceneInfo> = collection.listed_scenes().collect();
        let is_listed = |id: &SceneId| listed.iter().any(|s| &s.scene_id == id);
        for scene in &listed {
            if folder_of(collection, &scene.scene_id).as_ref() != parent
                || scene.variant_of.as_ref().is_some_and(is_listed)
            {
                continue;
            }
            let keyframes: Vec<&SceneInfo> = listed
                .iter()
                .filter(|k| k.variant_of.as_ref() == Some(&scene.scene_id))
                .copied()
                .collect();
            let has_folders = collection
                .folders
                .iter()
                .any(|f| f.scene_id.as_ref() == Some(&scene.scene_id));
            let mut under = Vec::new();
            self.family(scene, &keyframes, None, depth + 1, &mut under);
            if (self.shows)(&scene.name, scene.meta.favourite, &scene.meta.labels) {
                out.push(Row::scene(
                    scene,
                    depth,
                    collection.parent_of(&scene.scene_id),
                    !keyframes.is_empty() || !scene.timelines.is_empty() || has_folders,
                ));
            }
            if !self.folded.contains(&scene.scene_id) {
                out.extend(under);
            }
        }
    }

    /// What sits in `parent` under `scene` (the scene itself for `None`), at `depth`
    /// (ADR-0219): its folders there in folder order, each followed by its contents
    /// unless collapsed, then its keyframe scenes filed there in display order, then its
    /// sequences filed there.
    fn family(
        &mut self,
        scene: &SceneInfo,
        keyframes: &[&SceneInfo],
        parent: Option<&FolderId>,
        depth: usize,
        out: &mut Vec<Row>,
    ) {
        let collection = self.collection;
        let owner = Some(&scene.scene_id);
        for folder in &collection.folders {
            if folder.scene_id.as_ref() != owner
                || parent_of(collection, &folder.folder_id).as_ref() != parent
                || !self.seen.insert(folder.folder_id.clone())
            {
                continue;
            }
            let mut inner = Vec::new();
            self.family(
                scene,
                keyframes,
                Some(&folder.folder_id),
                depth + 1,
                &mut inner,
            );
            if self.filtering && inner.is_empty() {
                continue;
            }
            let open = self.filtering || !self.collapsed.contains(&folder.folder_id);
            out.push(Row::Folder {
                folder_id: folder.folder_id.clone(),
                name: folder.name.clone(),
                depth,
                open,
                kept: self.filtering,
            });
            if open {
                out.extend(inner);
            }
        }
        let here =
            |folder: Option<&FolderId>| filed_in(collection, folder, owner).as_ref() == parent;
        for keyframe in keyframes {
            if here(keyframe.folder.as_ref())
                && (self.shows)(
                    &keyframe.name,
                    keyframe.meta.favourite,
                    &keyframe.meta.labels,
                )
            {
                out.push(Row::scene(keyframe, depth, Some(scene), false));
            }
        }
        for timeline in &scene.timelines {
            let name = crate::panels::sequence::timeline_name(timeline);
            if here(timeline.folder.as_ref()) && (self.shows)(&name, false, &[]) {
                out.push(Row::Sequence {
                    timeline_id: timeline.timeline_id.clone(),
                    name,
                    depth,
                });
            }
        }
    }
}

/// The edits that move the row `moved` to the `side` of the row `target` (S19), as one
/// batch: a scene, keyframe, sequence or folder dropped into a folder goes in it;
/// dropped beside a row it joins that row's folder, and beside a row of its own kind it
/// also takes the position there (sequences keep their order by ID). A row moves only
/// among the rows of its own space (ADR-0219): the scene list's, or those under one
/// scene, so a keyframe or sequence stays under its scene. Nothing for a drop where it
/// already is, a drop into another space, or a folder dropped on itself or anything
/// within it.
fn moved(collection: &Collection, moved: &Node, target: &Node, side: Side) -> Vec<Edit> {
    // The folder the target's row sits in, or, for into, the target folder itself.
    let into = match (target, side) {
        (Node::Folder(folder), Side::Into) => Some(folder.clone()),
        (Node::Scene(_) | Node::Sequence(_), Side::Into) => return Vec::new(),
        (Node::Folder(folder), _) => parent_of(collection, folder),
        (Node::Scene(scene), _) => folder_of(collection, scene),
        (Node::Sequence(timeline), _) => sequence_folder(collection, timeline),
    };
    let mut edits = Vec::new();
    // The space the drop lands in: the target folder's for into, else the target row's.
    let lands = match (target, side) {
        (Node::Folder(folder), Side::Into) => collection.folder(folder).map(|f| f.scene_id.clone()),
        _ => space_of(collection, target),
    };
    match (space_of(collection, moved), lands) {
        (Some(from), Some(to)) if from == to => {}
        _ => return edits,
    }
    match moved {
        Node::Sequence(timeline_id) => {
            let Some((scene, timeline)) = collection.timeline(timeline_id) else {
                return edits;
            };
            if sequence_folder(collection, timeline_id) != into {
                edits.push(Edit::TimelineSet {
                    scene_id: scene.scene_id.clone(),
                    timeline: TimelineInfo {
                        folder: into,
                        ..timeline.clone()
                    },
                });
            }
        }
        Node::Scene(scene_id) => {
            if collection.scene(scene_id).is_none() {
                return edits;
            }
            if folder_of(collection, scene_id) != into {
                edits.push(Edit::SceneSetFolder {
                    scene_id: scene_id.clone(),
                    folder_id: into,
                });
            }
            if let Node::Scene(target) = target {
                let order: Vec<&SceneId> = collection.scenes.iter().map(|s| &s.scene_id).collect();
                if let Some(index) = dropped_at(&order, &scene_id, &target, side) {
                    edits.push(Edit::SceneReorder {
                        scene_id: scene_id.clone(),
                        index,
                    });
                }
            }
        }
        Node::Folder(folder_id) => {
            let lands_inside = into
                .as_ref()
                .is_some_and(|into| within(collection, into, folder_id));
            if collection.folder(folder_id).is_none() || lands_inside {
                return edits;
            }
            if parent_of(collection, folder_id) != into {
                edits.push(Edit::FolderSetParent {
                    folder_id: folder_id.clone(),
                    parent: into,
                });
            }
            if let Node::Folder(target) = target {
                let order: Vec<&FolderId> =
                    collection.folders.iter().map(|f| &f.folder_id).collect();
                if let Some(index) = dropped_at(&order, &folder_id, &target, side) {
                    edits.push(Edit::FolderReorder {
                        folder_id: folder_id.clone(),
                        index,
                    });
                }
            }
        }
    }
    edits
}

/// The edits that move `scene_id` one place `up` or down among the scenes of its own
/// folder, as the tool bar's arrows do (SC-11): nothing at either end, or for a keyframe,
/// which stays under its scene.
fn stepped(collection: &Collection, scene_id: &SceneId, up: bool) -> Vec<Edit> {
    if collection.is_keyframe(scene_id) {
        return Vec::new();
    }
    let folder = folder_of(collection, scene_id);
    let siblings: Vec<&SceneId> = collection
        .listed_scenes()
        .filter(|s| !collection.is_keyframe(&s.scene_id))
        .filter(|s| folder_of(collection, &s.scene_id) == folder)
        .map(|s| &s.scene_id)
        .collect();
    let Some(at) = siblings.iter().position(|s| *s == scene_id) else {
        return Vec::new();
    };
    let neighbour = if up { at.checked_sub(1) } else { Some(at + 1) };
    let Some(neighbour) = neighbour.and_then(|n| siblings.get(n)) else {
        return Vec::new();
    };
    let side = if up { Side::Before } else { Side::After };
    moved(
        collection,
        &Node::Scene(scene_id.clone()),
        &Node::Scene((*neighbour).clone()),
        side,
    )
}

/// Whether a press picks its scene row at once ([`Scenes`]): a mouse's primary button
/// with no modifier. Shift, which the reorder leaves alone, and Ctrl and Meta (on macOS a
/// Ctrl+click is the secondary click, which opens the row menu and sends no `click`)
/// pick on the click, as before. A touch or a pen picks on the click too, since its
/// press may start a scroll.
fn picks_on_press(pointer_type: &str, button: i16, modifiers: Modifiers) -> bool {
    pointer_type == "mouse" && button == 0 && modifiers == Modifiers::default()
}

/// The modifier keys held at a press.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Modifiers {
    shift: bool,
    ctrl: bool,
    meta: bool,
}

/// The name a new keyframe scene of a scene with `count` of them gets.
pub(crate) fn keyframe_name(count: usize) -> String {
    format!("Keyframe {}", count + 1)
}

/// The scene list.
#[component]
pub fn Scenes() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    // The row being renamed, by its key.
    let renaming = RwSignal::new(None::<String>);
    let pending = RwSignal::new(None::<Pending>);
    let filter = Filter::new();
    let reorder = Reorder::new();
    // The folders whose contents are hidden (S18), for this page only.
    let collapsed = RwSignal::new(HashSet::<FolderId>::new());
    // The scenes whose keyframes are folded away.
    let folded = RwSignal::new(HashSet::<SceneId>::new());

    let menus = use_context::<Menus>();
    // A scene dragged out of the list towards the Show panel's tracks.
    let show_panel = use_context::<crate::panels::show::ShowPanel>();
    let carry = move |event: &leptos::ev::PointerEvent, dropped: bool| {
        let Some(panel) = show_panel else { return };
        // Only a scene of the list is carried: a keyframe or a sequence stays here.
        let carried = reorder.dragged.get_untracked().map(|k| Node::of(&k));
        let Some(Node::Scene(scene)) = carried.filter(|node| {
            studio
                .collection
                .with_untracked(|c| c.as_ref().is_some_and(|c| space_of(c, node) == Some(None)))
        }) else {
            if dropped {
                panel.carry.set(None);
            }
            return;
        };
        let name = studio
            .collection
            .with_untracked(|c| {
                c.as_ref()
                    .and_then(|c| c.scene(&scene).map(|s| s.name.clone()))
            })
            .unwrap_or_default();
        panel.carry.set(Some(crate::panels::show::Carry {
            scene: scene.to_string(),
            source: None,
            name,
            x: f64::from(event.client_x()),
            y: f64::from(event.client_y()),
            dropped,
        }));
    };
    let dock = use_context::<DockState>();
    let scene_dialog = use_context::<SceneDialog>();
    let pasting = crate::clipboard::Pasting::from_page();
    let manager = use_context::<LabelManager>();
    let filters = use_context::<FilterDialogs>();
    let any_rows = move || {
        studio.collection.with(|c| {
            c.as_ref()
                .is_some_and(|c| !c.folders.is_empty() || c.listed_scenes().next().is_some())
        })
    };
    let shown_rows = move || {
        let filtering = filter.active();
        let shows =
            |name: &str, favourite: bool, labels: &[LabelId]| filter.shows(name, favourite, labels);
        collapsed.with(|collapsed| {
            folded.with(|folded| {
                studio.collection.with(|c| {
                    c.as_ref()
                        .map(|c| rows(c, collapsed, folded, filtering, &shows))
                        .unwrap_or_default()
                })
            })
        })
    };
    let send_all = move |edits: Vec<Edit>| {
        if let Some(s) = session.get()
            && !edits.is_empty()
        {
            studio.apply(&s, edits);
        }
    };
    let send = move |edit: Edit| send_all(vec![edit]);
    let open_folder = move |folder_id: &FolderId| {
        if collapsed.with_untracked(|c| c.contains(folder_id)) {
            collapsed.update(|c| {
                c.remove(folder_id);
            });
        }
    };
    // Adds a scene named "Scene N" in `folder`, selects it and opens its name for
    // editing (the menus' New scene).
    let new_scene = move |folder: Option<FolderId>| {
        let Some(s) = session.get() else { return };
        let count = studio
            .collection
            .with_untracked(|c| c.as_ref().map_or(0, |c| c.listed_scenes().count()));
        let scene_id = SceneId::new(studio.fresh_id(&s, "scene"));
        let mut edits = vec![Edit::SceneCreate {
            scene_id: scene_id.clone(),
            name: format!("Scene {}", count + 1),
            role: None,
        }];
        if let Some(folder) = &folder {
            open_folder(folder);
            edits.push(Edit::SceneSetFolder {
                scene_id: scene_id.clone(),
                folder_id: Some(folder.clone()),
            });
        }
        studio.apply(&s, edits);
        studio.select_scene(scene_id.clone());
        renaming.set(Some(scene_id.to_string()));
    };
    // Adds a folder named "New folder" in `parent`, with its name open for editing (the
    // menus' New folder): one of the scene list's, or with `scene` one under that scene
    // for its keyframes and sequences (ADR-0219), which unfolds.
    let new_folder = move |parent: Option<FolderId>, scene: Option<SceneId>| {
        let Some(s) = session.get() else { return };
        let folder_id = FolderId::new(studio.fresh_id(&s, "folder"));
        if let Some(parent) = &parent {
            open_folder(parent);
        }
        if let Some(scene) = &scene {
            folded.update(|f| _ = f.remove(scene));
        }
        studio.apply(
            &s,
            vec![Edit::FolderCreate {
                folder_id: folder_id.clone(),
                name: "New folder".into(),
                parent,
                scene_id: scene,
            }],
        );
        renaming.set(Some(format!("{FOLDER_KEY}{folder_id}")));
    };
    // Files keyframe `scene_id` in `folder`, one of its parent's, or takes it out.
    let file_keyframe = move |scene_id: SceneId, folder: Option<FolderId>| {
        if let Some(folder) = &folder {
            open_folder(folder);
        }
        send(Edit::SceneSetFolder {
            scene_id,
            folder_id: folder,
        });
    };
    // Files sequence `timeline_id` in `folder`, one of its scene's, or takes it out.
    let file_sequence = move |timeline_id: TimelineId, folder: Option<FolderId>| {
        let edit = studio.collection.with_untracked(|c| {
            let (scene, timeline) = c.as_ref()?.timeline(&timeline_id)?;
            (timeline.folder != folder).then(|| Edit::TimelineSet {
                scene_id: scene.scene_id.clone(),
                timeline: TimelineInfo {
                    folder: folder.clone(),
                    ..timeline.clone()
                },
            })
        });
        if let Some(folder) = &folder {
            open_folder(folder);
        }
        if let Some(edit) = edit {
            send(edit);
        }
    };
    // The "Move to folder" submenu of a keyframe or sequence under `owner`, now in
    // `current`: no folder, then each of the scene's folders, the current one checked.
    let move_to_folder =
        move |owner: SceneId,
              current: Option<FolderId>,
              file: std::rc::Rc<dyn Fn(Option<FolderId>)>| {
            let folders = studio.collection.with_untracked(|c| {
                c.as_ref()
                    .map(|c| scene_folders(c, &owner))
                    .unwrap_or_default()
            });
            if folders.is_empty() {
                return Entry::submenu("Move to folder", Vec::new());
            }
            let none = {
                let file = file.clone();
                Entry::item("No folder", move || file(None)).checked(current.is_none())
            };
            let entries = std::iter::once(none)
                .chain(folders.into_iter().map(|(folder_id, path)| {
                    let on = current.as_ref() == Some(&folder_id);
                    let file = file.clone();
                    Entry::item(path, move || file(Some(folder_id.clone()))).checked(on)
                }))
                .collect();
            Entry::submenu("Move to folder", entries)
        };

    let switch = move |scene_id: SceneId| {
        if let Some(s) = session.get() {
            studio.act(
                &s,
                Action::SceneSwitch {
                    scene_id,
                    preset_id: None,
                    via: Vec::new(),
                },
            );
        }
    };
    // A new keyframe scene of `parent`, in `folder` (one of the parent's, ADR-0219) if
    // given, unfolded and selected so it can be arranged.
    let new_keyframe = move |parent: SceneId, folder: Option<FolderId>| {
        let Some(s) = session.get() else { return };
        let count = studio
            .collection
            .with_untracked(|c| c.as_ref().map_or(0, |c| c.keyframes_of(&parent).count()));
        let scene_id = SceneId::new(studio.fresh_id(&s, "scene"));
        let mut edits = vec![Edit::SceneCreateVariant {
            scene_id: scene_id.clone(),
            variant_of: parent.clone(),
            name: keyframe_name(count),
        }];
        if let Some(folder) = &folder {
            open_folder(folder);
            edits.push(Edit::SceneSetFolder {
                scene_id: scene_id.clone(),
                folder_id: Some(folder.clone()),
            });
        }
        studio.apply(&s, edits);
        folded.update(|f| _ = f.remove(&parent));
        studio.select_scene(scene_id);
    };
    // Every item of the keyframe follows its parent again.
    let reset_keyframe = move |scene_id: SceneId| {
        let Some(s) = session.get() else { return };
        let edits = studio.collection.with_untracked(|c| {
            c.as_ref()
                .and_then(|c| c.scene(&scene_id))
                .map(reset_edits)
                .unwrap_or_default()
        });
        if !edits.is_empty() {
            studio.apply(&s, edits);
        }
    };
    // A keyframe holds no items, so it goes at once unless something refers to it,
    // which the plan clears after a confirmation as a scene's delete does. Going at
    // once, its parent is selected if it was.
    let delete_keyframe = move |scene_id: SceneId, parent: SceneId, name: String| {
        let Some(s) = session.get() else { return };
        spawn_local(async move {
            let target = DeleteTarget::Scene {
                scene_id: scene_id.clone(),
            };
            // An engine without `delete.plan` (the mock) gets the bare delete.
            let (rev, edits) = match plan_delete(&s, target).await {
                Ok((rev, edits)) => (Some(rev), edits),
                Err(_) => (
                    None,
                    vec![Edit::SceneDelete {
                        scene_id: scene_id.clone(),
                    }],
                ),
            };
            if let (Some(rev), true) = (rev, edits.len() > 1) {
                return pending.set(Some(Pending {
                    question: format!("Delete keyframe “{name}”?"),
                    detail: plan_summary(&edits),
                    rev,
                    edits,
                }));
            }
            if studio
                .selection
                .with_untracked(|s| s.scene.as_ref() == Some(&scene_id))
            {
                studio.select_scene(parent);
            }
            if let Err(error) = apply(&s, edits, rev).await {
                studio.report(&error);
            }
        });
    };

    let ask_delete = move |scene_id: SceneId, name: String| {
        let Some(s) = session.get() else { return };
        spawn_local(async move {
            match plan_delete(&s, DeleteTarget::Scene { scene_id }).await {
                Ok((rev, edits)) => pending.set(Some(Pending {
                    question: format!("Delete scene “{name}”?"),
                    detail: plan_summary(&edits),
                    rev,
                    edits,
                })),
                Err(error) => studio.report(&error),
            }
        });
    };
    // An empty folder goes at once (Ctrl+Z brings it back); one that holds anything asks
    // first, saying what its plan moves up a level.
    let ask_delete_folder = move |folder_id: FolderId, name: String| {
        let Some(s) = session.get() else { return };
        let empty = studio.collection.with_untracked(|c| {
            c.as_ref().is_some_and(|c| {
                !c.folders
                    .iter()
                    .any(|f| f.parent.as_ref() == Some(&folder_id))
                    && !c.scenes.iter().any(|sc| {
                        sc.folder.as_ref() == Some(&folder_id)
                            || sc
                                .timelines
                                .iter()
                                .any(|t| t.folder.as_ref() == Some(&folder_id))
                    })
            })
        });
        if empty {
            studio.apply(&s, vec![Edit::FolderDelete { folder_id }]);
            return;
        }
        spawn_local(async move {
            match plan_delete(&s, DeleteTarget::Folder { folder_id }).await {
                Ok((rev, edits)) => pending.set(Some(Pending {
                    question: format!("Delete folder “{name}”?"),
                    detail: plan_summary(&edits),
                    rev,
                    edits,
                })),
                Err(error) => studio.report(&error),
            }
        });
    };
    let confirm_delete = move |plan: Pending| {
        let Some(s) = session.get() else { return };
        spawn_local(async move {
            if let Err(error) = apply(&s, plan.edits, Some(plan.rev)).await {
                studio.report(&error);
            }
        });
    };
    // A dropped row moves as one batch; a folder it went into opens.
    let drop = move |dragged: String, target: String, side: Side| {
        let (dragged, target) = (Node::of(&dragged), Node::of(&target));
        let edits = studio.collection.with_untracked(|c| {
            c.as_ref()
                .map(|c| moved(c, &dragged, &target, side))
                .unwrap_or_default()
        });
        if let (Node::Folder(folder), Side::Into, false) = (&target, side, edits.is_empty()) {
            open_folder(folder);
        }
        send_all(edits);
    };

    // The parts every row has (S10): the indent, the drop marks and the drag.
    let drop_side = move |key: String, side: Side| {
        move || {
            reorder
                .target
                .with(|t| t.as_ref().is_some_and(|(id, s)| *id == key && *s == side))
        }
    };
    let dragging =
        move |key: String| move || reorder.dragged.with(|d| d.as_deref() == Some(key.as_str()));
    let press = move |key: String| {
        move |event: leptos::ev::PointerEvent| {
            if !filter.active() && renaming.with_untracked(Option::is_none) {
                reorder.down(&key, &event);
            }
        }
    };

    let scene_row = move |scene_id: SceneId,
                          name: String,
                          icon: &'static str,
                          favourite: bool,
                          depth: usize,
                          keyframe_of: Option<(SceneId, String)>,
                          keyframes: bool| {
        let is_keyframe = keyframe_of.is_some();
        let key = scene_id.to_string();
        let selected = {
            let id = scene_id.clone();
            move || studio.selection.with(|s| s.scene.as_ref() == Some(&id))
        };
        let on_program = {
            let id = scene_id.clone();
            Memo::new(move |_| studio.program.with(|p| p.as_ref() == Some(&id)))
        };
        let is_renaming = {
            let key = key.clone();
            move || renaming.with(|r| r.as_deref() == Some(key.as_str()))
        };
        // Picking the scene selects it, and puts it on program, or on preview in studio
        // mode. The action goes first, so it leaves before the panels redraw for the
        // new selection. A mouse picks on the press, as OBS does, so the switch doesn't
        // wait for the button's release (a click is about a tenth of a second); a touch
        // or a pen, which may be starting a scroll, and the keyboard pick on the click.
        let pick = {
            let id = scene_id.clone();
            move || {
                if let Some(s) = session.get() {
                    studio.act(&s, studio.click_action(&id));
                }
                studio.select_scene(id.clone());
            }
        };
        // Whether the last press picked, so its click doesn't pick again.
        let pressed = StoredValue::new(false);
        let pick_on_press = {
            let pick = pick.clone();
            move |event: leptos::ev::PointerEvent| {
                let picks = picks_on_press(
                    &event.pointer_type(),
                    event.button(),
                    Modifiers {
                        shift: event.shift_key(),
                        ctrl: event.ctrl_key(),
                        meta: event.meta_key(),
                    },
                );
                pressed.set_value(picks);
                if picks {
                    pick();
                }
            }
        };
        let select = {
            let pick = pick.clone();
            move |event: leptos::ev::MouseEvent| {
                // A keyboard's click has no presses (`detail` 0).
                let picked = std::mem::take(&mut *pressed.write_value()) && event.detail() > 0;
                if !picked {
                    pick();
                }
            }
        };
        let to_program = {
            let id = scene_id.clone();
            move |_| switch(id.clone())
        };
        let rename = {
            let id = scene_id.clone();
            let old = name.clone();
            move |to: String| {
                renaming.set(None);
                let to = to.trim().to_owned();
                if !to.is_empty() && to != old {
                    send(Edit::SceneRename {
                        scene_id: id.clone(),
                        name: to,
                    });
                }
            }
        };
        // A double click opens the scene's Properties, or renames a keyframe, which has
        // none.
        let on_double_click = {
            let id = scene_id.clone();
            let key = key.clone();
            move |_| match scene_dialog.filter(|_| !is_keyframe) {
                Some(dialog) => dialog.open(id.clone()),
                None => renaming.set(Some(key.clone())),
            }
        };
        // F2 renames and Del deletes, the keys the menu's hints name. Ctrl+C copies the
        // scene and Ctrl+V duplicates the copied one (KB-05).
        let on_key = {
            let id = scene_id.clone();
            let key = key.clone();
            let name = name.clone();
            let parent = keyframe_of.clone();
            move |event: leptos::ev::KeyboardEvent| match event.key().as_str() {
                "c" | "C" if event.ctrl_key() || event.meta_key() => {
                    if let Some(p) = pasting {
                        p.copy_scene(&id);
                    }
                }
                "v" | "V" if event.ctrl_key() || event.meta_key() => {
                    event.prevent_default();
                    if let Some(p) = pasting {
                        p.paste_scene();
                    }
                }
                "F2" => {
                    event.prevent_default();
                    renaming.set(Some(key.clone()));
                }
                "Delete" => {
                    event.prevent_default();
                    match &parent {
                        Some((parent, _)) => {
                            delete_keyframe(id.clone(), parent.clone(), name.clone());
                        }
                        None => ask_delete(id.clone(), name.clone()),
                    }
                }
                _ => {}
            }
        };
        let toggle_favourite = {
            let id = scene_id.clone();
            move || {
                let edit = studio.collection.with_untracked(|c| {
                    let scene = c.as_ref()?.scene(&id)?;
                    let mut meta = scene.meta.clone();
                    meta.favourite = !meta.favourite;
                    Some(Edit::SceneSetMeta {
                        scene_id: id.clone(),
                        meta,
                    })
                });
                if let Some(edit) = edit {
                    send(edit);
                }
            }
        };
        // The scene's menu (5.2.7). Rename and Delete scene work here, though the
        // prototype shows them disabled, and so does Show in preview. New scene and New
        // folder go where the scene is. A keyframe's menu is its own.
        let on_row_context = {
            let id = scene_id.clone();
            let key = key.clone();
            let name = name.clone();
            let parent = keyframe_of.clone();
            move |event: leptos::ev::MouseEvent| {
                let Some(menus) = menus else { return };
                let rename_key = key.clone();
                if let Some((parent, parent_name)) = parent.clone() {
                    let (switch_id, reset_id, delete_id) = (id.clone(), id.clone(), id.clone());
                    let new_parent = parent.clone();
                    let folder_parent = parent.clone();
                    let delete_name = name.clone();
                    let (overrides, folder) = studio.collection.with_untracked(|c| {
                        c.as_ref().map_or((false, None), |c| {
                            (
                                c.scene(&id).is_some_and(|s| !s.overrides.is_empty()),
                                folder_of(c, &id),
                            )
                        })
                    });
                    let (new_in, folder_in) = (folder.clone(), folder.clone());
                    let move_menu = {
                        let id = id.clone();
                        move_to_folder(
                            parent.clone(),
                            folder,
                            std::rc::Rc::new(move |to| file_keyframe(id.clone(), to)),
                        )
                    };
                    menus.open(
                        &event,
                        vec![
                            Entry::item("Switch to it", move || switch(switch_id.clone())),
                            Entry::item("Rename", move || renaming.set(Some(rename_key.clone())))
                                .hint("F2"),
                            Entry::Sep,
                            Entry::submenu("Transition in", Vec::new()),
                            Entry::submenu("Transition out", Vec::new()),
                            sequence::trigger_submenu(
                                studio,
                                session,
                                parent.clone(),
                                keyframe_watches(&parent, &id),
                            ),
                            Entry::Sep,
                            Entry::item(format!("Reset to “{parent_name}”"), move || {
                                reset_keyframe(reset_id.clone());
                            })
                            .disabled_if(!overrides),
                            Entry::item("New keyframe", move || {
                                new_keyframe(new_parent.clone(), new_in.clone());
                            }),
                            Entry::item("New folder", move || {
                                new_folder(folder_in.clone(), Some(folder_parent.clone()));
                            }),
                            move_menu,
                            Entry::Sep,
                            Entry::item("Delete keyframe", move || {
                                delete_keyframe(
                                    delete_id.clone(),
                                    parent.clone(),
                                    delete_name.clone(),
                                );
                            })
                            .hint("Del"),
                        ],
                    );
                    return;
                }
                let preview_id = id.clone();
                let props_id = id.clone();
                let keyframe_id = id.clone();
                let filters_id = id.clone();
                let (delete_id, delete_name) = (id.clone(), name.clone());
                let folder = studio
                    .collection
                    .with_untracked(|c| c.as_ref().and_then(|c| folder_of(c, &id)));
                let scene_folder = folder.clone();
                // Whether a multiview shows it, set here as in Scene properties (DL-03).
                // The meta is read on the click, so a change since the menu opened stays.
                let multiview = {
                    let id = id.clone();
                    let on = studio.collection.with_untracked(|c| {
                        c.as_ref()
                            .and_then(|c| c.scene(&id))
                            .is_some_and(|s| s.meta.show_in_multiview)
                    });
                    Entry::item("Show in multiview", move || {
                        let meta = studio.collection.with_untracked(|c| {
                            c.as_ref()
                                .and_then(|c| c.scene(&id))
                                .map(|s| s.meta.clone())
                        });
                        if let Some(mut meta) = meta {
                            meta.show_in_multiview = !meta.show_in_multiview;
                            send_all(vec![Edit::SceneSetMeta {
                                scene_id: id.clone(),
                                meta,
                            }]);
                        }
                    })
                    .checked(on)
                };
                // "Keep warm" (ADR-0117 §7), as in Scene properties. A keyframe scene is
                // shown through its parent, so it can't be kept warm itself.
                let keep_warm = {
                    let id = id.clone();
                    let (on, keyframe) = studio.collection.with_untracked(|c| {
                        c.as_ref()
                            .and_then(|c| c.scene(&id))
                            .map_or((false, true), |s| {
                                (s.options.keep_warm, s.variant_of.is_some())
                            })
                    });
                    Entry::item("Keep warm", move || {
                        let options = studio.collection.with_untracked(|c| {
                            c.as_ref()
                                .and_then(|c| c.scene(&id))
                                .map(|s| s.options.clone())
                        });
                        if let Some(mut options) = options {
                            options.keep_warm = !options.keep_warm;
                            send_all(vec![Edit::SceneSetOptions {
                                scene_id: id.clone(),
                                options,
                            }]);
                        }
                    })
                    .checked(on)
                    .disabled_if(keyframe)
                };
                let copy_id = id.clone();
                menus.open(
                    &event,
                    vec![
                        Entry::item("Show in preview", move || {
                            if let Some(s) = session.get() {
                                studio.act(
                                    &s,
                                    Action::StudioSetPreview {
                                        scene_id: preview_id.clone(),
                                    },
                                );
                            }
                        }),
                        Entry::item("Rename", move || renaming.set(Some(rename_key.clone())))
                            .hint("F2"),
                        Entry::Sep,
                        override_submenu(studio, session, dock, &id, true),
                        override_submenu(studio, session, dock, &id, false),
                        sequence::trigger_submenu(studio, session, id.clone(), scene_watches(&id)),
                        Entry::Sep,
                        Entry::item("Properties…", move || {
                            if let Some(dialog) = scene_dialog {
                                dialog.open(props_id.clone());
                            }
                        })
                        .hint("Double-click")
                        .disabled_if(scene_dialog.is_none()),
                        Entry::item("Edit filters…", move || {
                            show_filters(
                                filters,
                                dock,
                                FilterOwner::Scene {
                                    scene_id: filters_id.clone(),
                                },
                            );
                        }),
                        crate::projector::pop_out(studio, Some(id.clone())),
                        crate::screenshot::entry(
                            "Screenshot (scene)",
                            studio,
                            Some(session),
                            Some(crate::screenshot::Shot::Scene(id.clone())),
                        ),
                        multiview,
                        keep_warm,
                        Entry::Sep,
                        Entry::item("New scene", move || new_scene(scene_folder.clone())),
                        Entry::item("New keyframe", move || {
                            new_keyframe(keyframe_id.clone(), None)
                        }),
                        Entry::item("New folder", move || new_folder(folder.clone(), None)),
                        {
                            let id = id.clone();
                            Entry::item(format!("New folder under “{name}”"), move || {
                                new_folder(None, Some(id.clone()));
                            })
                        },
                        Entry::Sep,
                        Entry::item("Copy", move || {
                            if let Some(p) = pasting {
                                p.copy_scene(&copy_id);
                            }
                        })
                        .hint("Ctrl+C")
                        .disabled_if(pasting.is_none()),
                        Entry::item("Paste scene", move || {
                            if let Some(p) = pasting {
                                p.paste_scene();
                            }
                        })
                        .hint("Ctrl+V")
                        .disabled_if(!pasting.is_some_and(|p| p.clipboard.scene().is_some())),
                        {
                            let id = id.clone();
                            Entry::item("Duplicate scene", move || {
                                if let Some(p) = pasting {
                                    p.duplicate_scene(&id);
                                }
                            })
                            .disabled_if(pasting.is_none())
                        },
                        Entry::item("Delete scene", move || {
                            ask_delete(delete_id.clone(), delete_name.clone());
                        })
                        .hint("Del"),
                    ],
                );
            }
        };
        // The caret folds or unfolds the scene's keyframes.
        let caret = {
            let id = scene_id.clone();
            let open = {
                let id = id.clone();
                Memo::new(move |_| !folded.with(|f| f.contains(&id)))
            };
            let toggle = move |event: leptos::ev::MouseEvent| {
                event.stop_propagation();
                folded.update(|f| {
                    if !f.remove(&id) {
                        f.insert(id.clone());
                    }
                });
            };
            keyframes.then(|| {
                view! {
                    <button
                        type="button"
                        class="tree-fold"
                        aria-label="Keyframes"
                        aria-expanded=move || open.get().to_string()
                        on:click=toggle
                        on:pointerdown=|event| event.stop_propagation()
                    >
                        {move || if open.get() { "▾" } else { "▸" }}
                    </button>
                }
            })
        };
        let title = keyframe_of
            .as_ref()
            .map(|(_, parent)| format!("Keyframe of “{parent}”"));
        let press_key = key.clone();
        let shown_name = name.clone();
        let star_name = name.clone();
        let tag_name = name.clone();
        let labels = {
            let id = scene_id.clone();
            Signal::derive(move || {
                studio.collection.with(|c| {
                    c.as_ref()
                        .and_then(|c| Some(carried(c, &c.scene(&id)?.meta.labels)))
                        .unwrap_or_default()
                })
            })
        };
        let popover = TagPopover::new();
        let popover_target = Tagged::Scene(scene_id.clone());
        let popover_indent = indent(depth);
        view! {
            <li
                class="row tree-row"
                class:keyframe=is_keyframe
                title=title
                class:selected=selected
                class:drop-before=drop_side(key.clone(), Side::Before)
                class:drop-after=drop_side(key.clone(), Side::After)
                class:dragging=dragging(key.clone())
                style:padding-left=indent(depth)
                data-tree-id=key.clone()
                aria-current=move || on_program.get().then_some("true")
                on:contextmenu=on_row_context
                on:keydown=on_key
                on:pointerdown=press(press_key.clone())
            >
                <span class="tree-caret">{caret}</span>
                <Show
                    when=is_renaming
                    fallback=move || {
                        let name = shown_name.clone();
                        view! {
                            <button
                                class="row-main"
                                on:pointerdown=pick_on_press.clone()
                                on:click=select.clone()
                                on:dblclick=on_double_click.clone()
                            >
                                <span class="tree-icon" class:keyframe-icon=is_keyframe>
                                    <Icon name=icon size=15 />
                                </span>
                                <span class="tree-label">{name}</span>
                                <Show when=move || on_program.get()>
                                    <span class="tree-badge">"program"</span>
                                </Show>
                            </button>
                        }
                    }
                >
                    <RenameField value=name.clone() done=rename.clone() />
                </Show>
                <LabelChips labels=labels />
                <Show when=move || studio.studio_mode.get() && !on_program.get()>
                    <button
                        type="button"
                        class="hov tree-tool"
                        aria-label="Program"
                        title="Put this scene on program"
                        on:click=to_program.clone()
                    >
                        <Icon name="out" size=14 />
                    </button>
                </Show>
                <TagButton popover=popover name=tag_name />
                <Star name=star_name on=Signal::stored(favourite) toggle=toggle_favourite />
            </li>
            {move || popover.open.get().then(|| view! {
                <li class="tree-pop-row">
                    <LabelPopover popover=popover target=popover_target.clone() indent=popover_indent.clone() />
                </li>
            })}
        }
        .into_any()
    };

    let folder_row =
        move |folder_id: FolderId, name: String, depth: usize, open: bool, kept: bool| {
            let key = format!("{FOLDER_KEY}{folder_id}");
            let is_renaming = {
                let key = key.clone();
                move || renaming.with(|r| r.as_deref() == Some(key.as_str()))
            };
            let toggle = {
                let id = folder_id.clone();
                move |_| {
                    collapsed.update(|c| {
                        if !c.remove(&id) {
                            c.insert(id.clone());
                        }
                    });
                }
            };
            let rename = {
                let id = folder_id.clone();
                let old = name.clone();
                move |to: String| {
                    renaming.set(None);
                    let to = to.trim().to_owned();
                    if !to.is_empty() && to != old {
                        send(Edit::FolderRename {
                            folder_id: id.clone(),
                            name: to,
                        });
                    }
                }
            };
            let on_key = {
                let id = folder_id.clone();
                let key = key.clone();
                let name = name.clone();
                move |event: leptos::ev::KeyboardEvent| match event.key().as_str() {
                    "F2" => {
                        event.prevent_default();
                        renaming.set(Some(key.clone()));
                    }
                    "Delete" => {
                        event.prevent_default();
                        ask_delete_folder(id.clone(), name.clone());
                    }
                    _ => {}
                }
            };
            // The folder's menu (5.2.5). New scene and New folder go in it; Rename and
            // Delete folder work here, though the prototype shows them disabled.
            let on_row_context = {
                let id = folder_id.clone();
                let key = key.clone();
                let name = name.clone();
                move |event: leptos::ev::MouseEvent| {
                    let Some(menus) = menus else { return };
                    let (scene_in, folder_in) = (id.clone(), id.clone());
                    let rename_key = key.clone();
                    let (delete_id, delete_name) = (id.clone(), name.clone());
                    // A folder under a scene (ADR-0219) makes keyframes of that scene.
                    let owner = studio.collection.with_untracked(|c| {
                        c.as_ref()
                            .and_then(|c| c.folder(&id))
                            .and_then(|f| f.scene_id.clone())
                    });
                    let folder_owner = owner.clone();
                    let new_here = match owner {
                        Some(owner) => Entry::item("New keyframe", move || {
                            new_keyframe(owner.clone(), Some(scene_in.clone()));
                        }),
                        None => Entry::item("New scene", move || new_scene(Some(scene_in.clone()))),
                    };
                    menus.open(
                        &event,
                        vec![
                            new_here,
                            Entry::item("New folder", move || {
                                new_folder(Some(folder_in.clone()), folder_owner.clone());
                            }),
                            Entry::item("Rename", move || renaming.set(Some(rename_key.clone())))
                                .hint("F2"),
                            Entry::Sep,
                            Entry::item("Delete folder", move || {
                                ask_delete_folder(delete_id.clone(), delete_name.clone());
                            })
                            .hint("Del"),
                        ],
                    );
                }
            };
            let caret_label = format!("{} {name}", if open { "Collapse" } else { "Expand" });
            let shown_name = name.clone();
            view! {
                <li
                    class="row tree-row folder"
                    class:kept=kept
                    class:drop-before=drop_side(key.clone(), Side::Before)
                    class:drop-after=drop_side(key.clone(), Side::After)
                    class:drop-into=drop_side(key.clone(), Side::Into)
                    class:dragging=dragging(key.clone())
                    style:padding-left=indent(depth)
                    data-tree-id=key.clone()
                    data-tree-container="true"
                    on:contextmenu=on_row_context
                    on:keydown=on_key
                    on:pointerdown=press(key.clone())
                >
                    <button
                        type="button"
                        class="tree-caret"
                        aria-label=caret_label
                        aria-expanded=open.to_string()
                        disabled=kept
                        on:click=toggle
                    >
                        {if open { "▾" } else { "▸" }}
                    </button>
                    <Show
                        when=is_renaming
                        fallback=move || {
                            let name = shown_name.clone();
                            view! {
                                <button class="row-main">
                                    <Icon name="folder" size=15 />
                                    <span class="tree-label">{name}</span>
                                </button>
                            }
                        }
                    >
                        <RenameField value=name.clone() done=rename.clone() />
                    </Show>
                </li>
            }
            .into_any()
        };

    // The time now while a sequence plays, for the rows' elapsed time.
    let clock = RwSignal::new(crate::store::now_ms());
    if let Ok(handle) = set_interval_with_handle(
        move || {
            if studio.playing.with_untracked(|p| !p.is_empty()) {
                clock.try_set(crate::store::now_ms());
            }
        },
        std::time::Duration::from_millis(200),
    ) {
        on_cleanup(move || handle.clear());
    }
    // The sequence whose key is being set, by ID.
    let keying = RwSignal::new(None::<TimelineId>);
    let sequence_row = move |timeline_id: TimelineId, name: String, depth: usize| {
        let key = format!("{SEQUENCE_KEY}{timeline_id}");
        let id = StoredValue::new(timeline_id);
        let is_renaming = {
            let key = key.clone();
            move || renaming.with(|r| r.as_deref() == Some(key.as_str()))
        };
        let selected = move || {
            studio
                .sequence
                .with(|s| id.with_value(|id| s.as_ref() == Some(id)))
        };
        let play = Action::TimelinePlay {
            timeline_id: id.get_value(),
            delay_ms: None,
        };
        let play_for_chords = play.clone();
        let chords = Memo::new(move |_| {
            studio.collection.with(|c| {
                c.as_ref()
                    .map(|c| c.chords_for(&play_for_chords))
                    .unwrap_or_default()
            })
        });
        let elapsed = move || {
            let playing = studio
                .playing
                .with(|p| id.with_value(|id| p.get(id).copied()))?;
            let duration = studio.collection.with(|c| {
                c.as_ref().and_then(|c| {
                    id.with_value(|id| c.timeline(id))
                        .map(|(_, t)| t.duration_ms)
                })
            })?;
            let ms = playing.position_at(clock.get(), duration);
            Some(format!("▶ {:.1} s", f64::from(ms) / 1000.0))
        };
        let open = move || crate::panels::sequence::open_sequence(studio, id.get_value());
        let toggle_play = {
            let play = play.clone();
            move || {
                let Some(s) = session.get() else { return };
                let on = studio
                    .playing
                    .with_untracked(|p| id.with_value(|id| p.contains_key(id)));
                if on {
                    studio.act(
                        &s,
                        Action::TimelineStop {
                            timeline_id: id.get_value(),
                        },
                    );
                } else {
                    studio.act(&s, play.clone());
                }
            }
        };
        let rename = {
            let old = name.clone();
            move |to: String| {
                renaming.set(None);
                let to = to.trim().to_owned();
                if to.is_empty() || to == old {
                    return;
                }
                let found = studio.collection.with_untracked(|c| {
                    c.as_ref().and_then(|c| {
                        id.with_value(|id| c.timeline(id))
                            .map(|(scene, t)| (scene.scene_id.clone(), t.clone()))
                    })
                });
                if let Some((scene_id, mut timeline)) = found {
                    timeline.name = to;
                    send(Edit::TimelineSet { scene_id, timeline });
                }
            }
        };
        let delete = move || {
            send(Edit::TimelineDelete {
                timeline_id: id.get_value(),
            });
        };
        let set_key = {
            move |chord: Option<Chord>| {
                keying.set(None);
                let Some(chord) = chord else { return };
                let play = Action::TimelinePlay {
                    timeline_id: id.get_value(),
                    delay_ms: None,
                };
                let edits = studio.collection.with_untracked(|c| {
                    c.as_ref()
                        .map(|c| crate::hotkeys::bind_edits(c, &play, &chord))
                        .unwrap_or_default()
                });
                send_all(edits);
            }
        };
        let clear_keys = move || {
            let play = Action::TimelinePlay {
                timeline_id: id.get_value(),
                delay_ms: None,
            };
            let edits = studio.collection.with_untracked(|c| {
                c.as_ref()
                    .map(|c| {
                        chords
                            .get_untracked()
                            .iter()
                            .flat_map(|chord| c.unbind_edits(chord, &play))
                            .collect()
                    })
                    .unwrap_or_default()
            });
            send_all(edits);
        };
        let on_row_context = {
            let key = key.clone();
            let toggle_play = toggle_play.clone();
            move |event: leptos::ev::MouseEvent| {
                event.prevent_default();
                event.stop_propagation();
                let Some(menus) = menus else { return };
                let playing = studio
                    .playing
                    .with_untracked(|p| id.with_value(|id| p.contains_key(id)));
                let has_keys = !chords.get_untracked().is_empty();
                let key = key.clone();
                let toggle_play = toggle_play.clone();
                // Its scene's folders (ADR-0219): a keyframe scene's sequence files in
                // its parent's.
                let filed = studio.collection.with_untracked(|c| {
                    let c = c.as_ref()?;
                    let (scene, _) = id.with_value(|id| c.timeline(id))?;
                    let owner = scene
                        .variant_of
                        .clone()
                        .unwrap_or_else(|| scene.scene_id.clone());
                    Some((owner, id.with_value(|id| sequence_folder(c, id))))
                });
                let folder_entries = filed.map(|(owner, folder)| {
                    let new_owner = owner.clone();
                    let new_in = folder.clone();
                    [
                        Entry::item("New folder", move || {
                            new_folder(new_in.clone(), Some(new_owner.clone()));
                        }),
                        move_to_folder(
                            owner,
                            folder,
                            std::rc::Rc::new(move |to| file_sequence(id.get_value(), to)),
                        ),
                    ]
                });
                menus.open(
                    &event,
                    vec![
                        Entry::item(if playing { "Stop" } else { "Play" }, toggle_play),
                        Entry::item("Open in Sequence", open),
                        Entry::Sep,
                        Entry::item("Rename", move || renaming.set(Some(key.clone()))).hint("F2"),
                        Entry::item("Set key…", move || keying.set(Some(id.get_value()))),
                        Entry::item("Remove key", clear_keys).disabled_if(!has_keys),
                        Entry::Sep,
                    ]
                    .into_iter()
                    .chain(folder_entries.into_iter().flatten())
                    .chain([
                        Entry::Sep,
                        Entry::item("Delete sequence", delete).hint("Del").danger(),
                    ])
                    .collect(),
                );
            }
        };
        let on_key = {
            let key = key.clone();
            move |event: leptos::ev::KeyboardEvent| match event.key().as_str() {
                "F2" => {
                    event.prevent_default();
                    renaming.set(Some(key.clone()));
                }
                "Delete" => {
                    event.prevent_default();
                    delete();
                }
                _ => {}
            }
        };
        let is_keying = move || keying.with(|k| id.with_value(|id| k.as_ref() == Some(id)));
        let shown_name = name.clone();
        view! {
            <li
                class="row tree-row seq-tree-row"
                class:selected=selected
                class:drop-before=drop_side(key.clone(), Side::Before)
                class:drop-after=drop_side(key.clone(), Side::After)
                class:dragging=dragging(key.clone())
                data-tree-id=key.clone()
                on:pointerdown=press(key.clone())
                style:padding-left=indent(depth)
                title="Sequence: click to open it, right-click for its menu"
                on:contextmenu=on_row_context
                on:keydown=on_key
            >
                <span class="tree-caret"></span>
                <Show
                    when=is_renaming
                    fallback=move || {
                        let name = shown_name.clone();
                        view! {
                            <button
                                class="row-main"
                                on:click=move |_| open()
                                on:dblclick=move |_| open()
                            >
                                <span class="tree-icon seq-icon">
                                    <Icon name="sequence" size=15 />
                                </span>
                                <span class="tree-label">{name}</span>
                                {move || elapsed().map(|t| view! { <span class="tree-badge seq-badge">{t}</span> })}
                            </button>
                        }
                    }
                >
                    <RenameField value=name.clone() done=rename.clone() />
                </Show>
                <Show
                    when=is_keying
                    fallback=move || view! {
                        <span class="seq-keys">
                            <For each=move || chords.get() key=|c| c.clone() children=|chord| view! {
                                <kbd class="seq-key">{crate::hotkeys::label(&chord)}</kbd>
                            } />
                        </span>
                    }
                >
                    <span class="seq-capture">
                        <crate::panels::learn::KeyCapture done=set_key />
                    </span>
                </Show>
                <button
                    type="button"
                    class="hov tree-tool"
                    aria-label=move || if studio.playing.with(|p| id.with_value(|id| p.contains_key(id))) { "Stop" } else { "Play" }
                    title=move || if studio.playing.with(|p| id.with_value(|id| p.contains_key(id))) { "Stop this sequence" } else { "Play this sequence" }
                    on:click=move |_| toggle_play()
                >
                    {move || if studio.playing.with(|p| id.with_value(|id| p.contains_key(id))) { "■" } else { "▶" }}
                </button>
            </li>
        }
        .into_any()
    };

    let row = move |row: Row| match row {
        Row::Sequence {
            timeline_id,
            name,
            depth,
        } => sequence_row(timeline_id, name, depth),
        Row::Folder {
            folder_id,
            name,
            depth,
            open,
            kept,
        } => folder_row(folder_id, name, depth, open, kept),
        Row::Scene {
            scene_id,
            name,
            icon,
            favourite,
            depth,
            keyframe_of,
            keyframes,
        } => scene_row(
            scene_id,
            name,
            icon,
            favourite,
            depth,
            keyframe_of,
            keyframes,
        ),
    };

    // The panel's own menu, on its empty space (5.2.3).
    let on_context = move |event: leptos::ev::MouseEvent| {
        let Some(menus) = menus else { return };
        menus.open(
            &event,
            vec![
                Entry::item("New scene", move || new_scene(None)),
                Entry::item("New folder", move || new_folder(None, None)),
                Entry::Sep,
                Entry::item("Paste scene", move || {
                    if let Some(p) = pasting {
                        p.paste_scene();
                    }
                })
                .hint("Ctrl+V")
                .disabled_if(!pasting.is_some_and(|p| p.clipboard.scene().is_some())),
            ],
        );
    };

    // The tool bar (SC-11) acts on the selected scene.
    let selected = move || studio.selection.with(|s| s.scene.clone());
    let selected_keyframe = move || {
        selected().is_some_and(|id| {
            studio
                .collection
                .with(|c| c.as_ref().is_some_and(|c| c.is_keyframe(&id)))
        })
    };
    let step = move |up: bool| {
        let Some(scene_id) = selected() else { return };
        let edits = studio.collection.with_untracked(|c| {
            c.as_ref()
                .map(|c| stepped(c, &scene_id, up))
                .unwrap_or_default()
        });
        send_all(edits);
    };
    let can_step = move |up: bool| {
        selected().is_some_and(|id| {
            studio
                .collection
                .with(|c| c.as_ref().is_some_and(|c| !stepped(c, &id, up).is_empty()))
        })
    };
    let tools = vec![
        Tool::new(
            "Add scene",
            "plus",
            move || studio.collection.with(Option::is_none),
            move || {
                // A keyframe sits under its parent: the new scene goes in the parent's folder.
                let folder = selected().and_then(|id| {
                    studio.collection.with_untracked(|c| {
                        c.as_ref().and_then(|c| {
                            let id = c.parent_of(&id).map_or(&id, |p| &p.scene_id);
                            folder_of(c, id)
                        })
                    })
                });
                new_scene(folder);
            },
        ),
        Tool::new(
            "Delete scene",
            "trash",
            move || selected().is_none(),
            move || {
                let Some(id) = selected() else { return };
                let found = studio.collection.with_untracked(|c| {
                    let c = c.as_ref()?;
                    let scene = c.scene(&id)?;
                    Some((
                        scene.name.clone(),
                        c.parent_of(&id).map(|p| p.scene_id.clone()),
                    ))
                });
                match found {
                    Some((name, Some(parent))) => delete_keyframe(id, parent, name),
                    Some((name, None)) => ask_delete(id, name),
                    None => {}
                }
            },
        ),
        Tool::new(
            "Scene properties",
            "gear",
            move || scene_dialog.is_none() || selected().is_none() || selected_keyframe(),
            move || {
                if let (Some(dialog), Some(id)) = (scene_dialog, selected()) {
                    dialog.open(id);
                }
            },
        ),
        Tool::new(
            "Scene filters",
            "sliders",
            // A keyframe has no chain of its own: the graph refuses filter edits there.
            move || selected().is_none() || selected_keyframe(),
            move || {
                if let Some(scene_id) = selected() {
                    show_filters(filters, dock, FilterOwner::Scene { scene_id });
                }
            },
        ),
        Tool::new("Move up", "up", move || !can_step(true), move || step(true)),
        Tool::new(
            "Move down",
            "down",
            move || !can_step(false),
            move || step(false),
        ),
    ];

    view! {
        <section class="panel scenes-panel tree-panel" on:contextmenu=on_context>
            <h2>"Scenes"</h2>
            <SearchBar what="scenes" filter=filter labels=PanelId::Scenes />
            {move || manager.is_some_and(|m| m.shown_in(PanelId::Scenes)).then(|| view! { <LabelEditor /> })}
            <Confirm pending=pending confirm=confirm_delete />
            <ul
                class="rows tree-rows"
                class:reordering=move || reorder.dragged.with(Option::is_some)
                on:pointermove=move |event| {
                    reorder.moved(&event);
                    carry(&event, false);
                }
                on:pointerup=move |event| {
                    carry(&event, true);
                    reorder.up(drop);
                }
                on:pointercancel=move |_| {
                    if let Some(panel) = show_panel {
                        panel.carry.set(None);
                    }
                    reorder.cancel();
                }
            >
                <For each=shown_rows key=|row| row.clone() children=row />
            </ul>
            {move || {
                let none = !any_rows();
                let hidden = !none && shown_rows().is_empty();
                (studio.collection.with(Option::is_some) && none)
                    .then(|| view! {
                        <p class="placeholder">"No scenes yet. Click + below to add one."</p>
                    }.into_any())
                    .or_else(|| {
                        hidden.then(|| view! { <p class="placeholder">"Nothing matches these filters."</p> }.into_any())
                    })
            }}
            <ToolBar label="Scene tools" tools=tools />
        </section>
    }
}

/// A text field that edits a name in place: Enter or leaving the field commits, Escape
/// cancels (commits the old name, which changes nothing).
#[component]
pub fn RenameField(value: String, done: impl Fn(String) + Clone + 'static) -> impl IntoView {
    let text = RwSignal::new(value.clone());
    // Enter commits and then the field's removal blurs it: commit once. By that blur the
    // component is disposed, so its handles are read with `try_`, and a gone one means
    // done.
    let finished = StoredValue::new(false);
    let finish = move |name: String| {
        if finished.try_get_value() == Some(false) {
            finished.set_value(true);
            done(name);
        }
    };
    let commit = {
        let finish = finish.clone();
        move || {
            if let Some(name) = text.try_get_untracked() {
                finish(name);
            }
        }
    };
    let on_blur = commit.clone();
    // `autofocus` only works for the page's first one, so focus the field once it is in
    // the page: it may appear after a menu closes, or after a new scene arrives.
    let field = NodeRef::<leptos::html::Input>::new();
    field.on_load(|input| {
        request_animation_frame(move || {
            let _ = input.focus();
            input.select();
        });
    });
    view! {
        <input
            class="rename"
            type="text"
            node_ref=field
            bind:value=text
            on:blur=move |_| on_blur()
            on:keydown=move |event| {
                // The keys are the field's: the row's own F2 and Del must not see them.
                event.stop_propagation();
                match event.key().as_str() {
                    "Enter" => commit(),
                    "Escape" => finish(value.clone()),
                    _ => {}
                }
            }
        />
    }
}

/// The triggers a scene's menu offers to start a sequence on (A14): the scene going on
/// program, and leaving it.
pub fn scene_watches(scene_id: &SceneId) -> Vec<(&'static str, TimelineTrigger)> {
    vec![
        (
            "Play when it goes on program",
            TimelineTrigger::Scene {
                scene_id: scene_id.clone(),
                edge: SceneEdge::Shown,
                delay_ms: 0,
            },
        ),
        (
            "Play when it leaves program",
            TimelineTrigger::Scene {
                scene_id: scene_id.clone(),
                edge: SceneEdge::Hidden,
                delay_ms: 0,
            },
        ),
    ]
}

/// The triggers a keyframe's menu offers to start a sequence on (A14): the keyframe of
/// `parent` loading, and leaving.
pub fn keyframe_watches(
    parent: &SceneId,
    keyframe_id: &SceneId,
) -> Vec<(&'static str, TimelineTrigger)> {
    let watch = |edge| TimelineTrigger::Keyframe {
        scene_id: parent.clone(),
        keyframe_id: Some(keyframe_id.clone()),
        edge,
        delay_ms: 0,
    };
    vec![
        ("Play when it loads", watch(KeyframeEdge::Loads)),
        ("Play when it leaves", watch(KeyframeEdge::Leaves)),
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        Modifiers, Node, Row, keyframe_name, moved, picks_on_press, rows, scene_folders, stepped,
    };
    use crate::panels::tree::Side;
    use crate::store::Collection;
    use jstream_control::protocol::{
        Edit, FolderId, FolderInfo, LabelId, SceneId, SceneInfo, SceneRole,
    };
    use std::collections::HashSet;

    fn scene(id: &str, folder: Option<&str>, favourite: bool) -> SceneInfo {
        let meta = jstream_control::protocol::SceneMeta {
            favourite,
            ..Default::default()
        };
        SceneInfo {
            scene_id: id.into(),
            name: id.to_uppercase(),
            role: SceneRole::Scene,
            transition_in: None,
            transition_out: None,
            folder: folder.map(FolderId::new),
            meta,
            options: Default::default(),
            variant_of: None,
            overrides: Vec::new(),
            items: Vec::new(),
            filters: Default::default(),
            camera: None,
            camera_anchor: None,
            camera_motion: Vec::new(),
            ptz_shots: Vec::new(),
            camera_follow: None,
            timelines: Vec::new(),
        }
    }

    fn folder(id: &str, parent: Option<&str>) -> FolderInfo {
        FolderInfo {
            scene_id: None,
            folder_id: id.into(),
            name: id.to_uppercase(),
            parent: parent.map(FolderId::new),
        }
    }

    /// Show (with Overlays inside it) holds a and b; BRB sits at the top level, and a
    /// group in Show is hidden. The inner folder comes after Show in folder order, and a
    /// scene names a folder that doesn't exist.
    fn collection() -> Collection {
        let mut group = scene("grp", Some("show"), false);
        group.role = SceneRole::Group;
        Collection {
            scenes: vec![
                scene("a", Some("show"), true),
                scene("brb", None, false),
                group,
                scene("b", Some("overlays"), false),
                scene("lost", Some("gone"), false),
            ],
            folders: vec![folder("show", None), folder("overlays", Some("show"))],
            ..Collection::default()
        }
    }

    /// The rows as `name@depth`, a folder's with `/` (and `+` when kept, `-` when shut).
    fn shown(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Folder {
                    name,
                    depth,
                    open,
                    kept,
                    ..
                } => {
                    let mark = if *kept {
                        "+"
                    } else if *open {
                        ""
                    } else {
                        "-"
                    };
                    format!("{name}/{mark}@{depth}")
                }
                Row::Scene { name, depth, .. } => format!("{name}@{depth}"),
                Row::Sequence { name, depth, .. } => format!("~{name}@{depth}"),
            })
            .collect()
    }

    #[test]
    fn folders_come_first_at_each_level_and_hold_their_scenes() {
        let all = |_: &str, _: bool, _: &[LabelId]| true;
        let c = collection();
        assert_eq!(
            shown(&rows(&c, &HashSet::new(), &HashSet::new(), false, &all)),
            ["SHOW/@0", "OVERLAYS/@1", "B@2", "A@1", "BRB@0", "LOST@0"]
        );
        // A collapsed folder hides what is in it, at any depth.
        let shut: HashSet<FolderId> = [FolderId::new("overlays")].into();
        assert_eq!(
            shown(&rows(&c, &shut, &HashSet::new(), false, &all)),
            ["SHOW/@0", "OVERLAYS/-@1", "A@1", "BRB@0", "LOST@0"]
        );
        let shut: HashSet<FolderId> = [FolderId::new("show")].into();
        assert_eq!(
            shown(&rows(&c, &shut, &HashSet::new(), false, &all)),
            ["SHOW/-@0", "BRB@0", "LOST@0"]
        );
    }

    #[test]
    fn a_filter_keeps_the_folders_of_matches_open_and_drops_the_rest() {
        let c = collection();
        // Collapsed or not, a folder holding a match shows open, kept.
        let shut: HashSet<FolderId> = [FolderId::new("show")].into();
        let b_only = |name: &str, _: bool, _: &[LabelId]| name == "B";
        assert_eq!(
            shown(&rows(&c, &shut, &HashSet::new(), true, &b_only)),
            ["SHOW/+@0", "OVERLAYS/+@1", "B@2"]
        );
        let favourites = |_: &str, favourite: bool, _: &[LabelId]| favourite;
        assert_eq!(
            shown(&rows(&c, &shut, &HashSet::new(), true, &favourites)),
            ["SHOW/+@0", "A@1"]
        );
        let nothing = |_: &str, _: bool, _: &[LabelId]| false;
        assert!(rows(&c, &shut, &HashSet::new(), true, &nothing).is_empty());
    }

    #[test]
    fn a_parent_loop_shows_nothing_twice() {
        let mut c = collection();
        c.folders = vec![folder("x", Some("y")), folder("y", Some("x"))];
        let all = |_: &str, _: bool, _: &[LabelId]| true;
        // Neither folder is at the top level, so neither shows; the walk ends.
        let shown = shown(&rows(&c, &HashSet::new(), &HashSet::new(), false, &all));
        assert!(
            !shown
                .iter()
                .any(|r| r.starts_with('X') || r.starts_with('Y'))
        );
    }

    fn scene_node(id: &str) -> Node {
        Node::Scene(SceneId::new(id))
    }

    fn folder_node(id: &str) -> Node {
        Node::Folder(FolderId::new(id))
    }

    #[test]
    fn a_scene_dropped_in_or_beside_a_row_joins_its_folder() {
        let c = collection();
        // Into a folder: only the folder changes.
        assert_eq!(
            moved(&c, &scene_node("brb"), &folder_node("overlays"), Side::Into),
            [Edit::SceneSetFolder {
                scene_id: "brb".into(),
                folder_id: Some("overlays".into()),
            }]
        );
        // Beside a scene in another folder: its folder, then its place in the order.
        assert_eq!(
            moved(&c, &scene_node("brb"), &scene_node("a"), Side::Before),
            [
                Edit::SceneSetFolder {
                    scene_id: "brb".into(),
                    folder_id: Some("show".into()),
                },
                Edit::SceneReorder {
                    scene_id: "brb".into(),
                    index: 0,
                },
            ]
        );
        // Beside a folder: that folder's parent, here the top level.
        assert_eq!(
            moved(&c, &scene_node("a"), &folder_node("show"), Side::After),
            [Edit::SceneSetFolder {
                scene_id: "a".into(),
                folder_id: None,
            }]
        );
        // Where it is already: nothing; and a scene holds nothing to drop into.
        assert!(moved(&c, &scene_node("a"), &folder_node("show"), Side::Into).is_empty());
        assert!(moved(&c, &scene_node("a"), &scene_node("brb"), Side::Into).is_empty());
    }

    #[test]
    fn the_arrows_move_a_scene_one_place_among_its_folders_scenes() {
        let mut keyframe = scene("kf", None, false);
        keyframe.variant_of = Some("y".into());
        let c = Collection {
            scenes: vec![
                scene("x", None, false),
                scene("a", Some("show"), false),
                scene("y", None, false),
                keyframe,
                scene("z", None, false),
            ],
            folders: vec![folder("show", None)],
            ..Collection::default()
        };
        let y = SceneId::new("y");
        // Up passes over the scene in a folder to the top-level scene above.
        assert_eq!(
            stepped(&c, &y, true),
            [Edit::SceneReorder {
                scene_id: "y".into(),
                index: 0,
            }]
        );
        // Down passes over its own keyframe to the next top-level scene.
        assert_eq!(
            stepped(&c, &y, false),
            [Edit::SceneReorder {
                scene_id: "y".into(),
                index: 4,
            }]
        );
        // Nothing at the ends, and a keyframe stays under its scene.
        assert!(stepped(&c, &SceneId::new("x"), true).is_empty());
        assert!(stepped(&c, &SceneId::new("z"), false).is_empty());
        assert!(stepped(&c, &SceneId::new("kf"), true).is_empty());
        // Alone in its folder, a scene has nowhere to go.
        assert!(stepped(&c, &SceneId::new("a"), false).is_empty());
    }

    #[test]
    fn a_folder_moves_but_never_into_itself() {
        let c = collection();
        // Out to the top level, beside BRB.
        assert_eq!(
            moved(
                &c,
                &folder_node("overlays"),
                &scene_node("brb"),
                Side::After
            ),
            [Edit::FolderSetParent {
                folder_id: "overlays".into(),
                parent: None,
            }]
        );
        // Before Show: the top level, and first in folder order.
        assert_eq!(
            moved(
                &c,
                &folder_node("overlays"),
                &folder_node("show"),
                Side::Before
            ),
            [
                Edit::FolderSetParent {
                    folder_id: "overlays".into(),
                    parent: None,
                },
                Edit::FolderReorder {
                    folder_id: "overlays".into(),
                    index: 0,
                },
            ]
        );
        // Into itself, into a folder within it, or beside a row within it: nothing.
        assert!(moved(&c, &folder_node("show"), &folder_node("show"), Side::Into).is_empty());
        assert!(
            moved(
                &c,
                &folder_node("show"),
                &folder_node("overlays"),
                Side::Into
            )
            .is_empty()
        );
        assert!(moved(&c, &folder_node("show"), &scene_node("b"), Side::Before).is_empty());
    }

    #[test]
    fn keyframes_nest_under_their_scene_and_fold_away() {
        let mut k2 = scene("k2", Some("show"), false);
        k2.variant_of = Some("b".into());
        let mut k1 = scene("k1", None, false);
        k1.variant_of = Some("b".into());
        let c = Collection {
            scenes: vec![k2, scene("a", None, false), scene("b", None, false), k1],
            ..Collection::default()
        };
        let all = |_: &str, _: bool, _: &[LabelId]| true;
        let none = HashSet::new();
        let at = |folded: &HashSet<SceneId>| shown(&rows(&c, &none, folded, false, &all));
        // Under their scene, a level in, wherever their own folder says they are.
        assert_eq!(at(&HashSet::new()), ["A@0", "B@0", "K2@1", "K1@1"]);
        let rows_now = rows(&c, &none, &HashSet::new(), false, &all);
        let Row::Scene {
            keyframes: true, ..
        } = &rows_now[1]
        else {
            panic!("B has keyframes: {rows_now:?}");
        };
        let Row::Scene {
            icon, keyframe_of, ..
        } = &rows_now[2]
        else {
            panic!("K2 is a scene row");
        };
        assert_eq!(*icon, "keyframe");
        assert_eq!(*keyframe_of, Some((SceneId::new("b"), "B".to_owned())));
        assert_eq!(at(&HashSet::from([SceneId::new("b")])), ["A@0", "B@0"]);
        // A filter keeps the keyframes that match, under their scene or not.
        let k1_only = |name: &str, _: bool, _: &[LabelId]| name == "K1";
        assert_eq!(
            shown(&rows(&c, &none, &HashSet::new(), true, &k1_only)),
            ["K1@1"]
        );
        // Keyframes don't move, and nothing lands on one.
        assert!(moved(&c, &scene_node("k1"), &scene_node("a"), Side::Before).is_empty());
        assert!(moved(&c, &scene_node("a"), &scene_node("k1"), Side::After).is_empty());
        assert_eq!(keyframe_name(2), "Keyframe 3");
    }

    /// A scene's sequences follow its keyframes, a level in, fold with them, and give
    /// the scene a caret on their own.
    #[test]
    fn sequences_follow_the_keyframes_and_fold_with_them() {
        let mut k1 = scene("k1", None, false);
        k1.variant_of = Some("b".into());
        let mut b = scene("b", None, false);
        b.timelines = vec![crate::panels::sequence::empty_sequence(
            "lights_up".into(),
            "Lights up".into(),
        )];
        let mut a = scene("a", None, false);
        a.timelines = vec![crate::panels::sequence::empty_sequence(
            "seq_1".into(),
            String::new(),
        )];
        let c = Collection {
            scenes: vec![a, b, k1],
            ..Collection::default()
        };
        let all = |_: &str, _: bool, _: &[LabelId]| true;
        let none = HashSet::new();
        assert_eq!(
            shown(&rows(&c, &none, &HashSet::new(), false, &all)),
            ["A@0", "~seq_1@1", "B@0", "K1@1", "~Lights up@1"]
        );
        let rows_now = rows(&c, &none, &HashSet::new(), false, &all);
        let Row::Scene {
            keyframes: true, ..
        } = &rows_now[0]
        else {
            panic!("A has a sequence, so a caret: {rows_now:?}");
        };
        assert_eq!(
            shown(&rows(
                &c,
                &none,
                &HashSet::from([SceneId::new("b")]),
                false,
                &all
            )),
            ["A@0", "~seq_1@1", "B@0"]
        );
        let lights = |name: &str, _: bool, _: &[LabelId]| name == "Lights up";
        assert_eq!(
            shown(&rows(&c, &none, &HashSet::new(), true, &lights)),
            ["~Lights up@1"]
        );
    }

    /// Scene B with its own folders (ADR-0219): Looks, with Intro inside it, holding
    /// keyframe K1 and sequence Lights up; keyframe K2 and sequence Fade unfiled; and a
    /// scene-list folder Show holding A.
    fn scene_folder_collection() -> Collection {
        let mut b = scene("b", None, false);
        let mut lights =
            crate::panels::sequence::empty_sequence("lights_up".into(), "Lights up".into());
        lights.folder = Some("intro".into());
        b.timelines = vec![
            crate::panels::sequence::empty_sequence("fade".into(), "Fade".into()),
            lights,
        ];
        let mut k1 = scene("k1", Some("intro"), false);
        k1.variant_of = Some("b".into());
        let mut k2 = scene("k2", None, false);
        k2.variant_of = Some("b".into());
        let owned = |id: &str, parent: Option<&str>| FolderInfo {
            scene_id: Some("b".into()),
            ..folder(id, parent)
        };
        Collection {
            scenes: vec![scene("a", Some("show"), false), b, k1, k2],
            folders: vec![
                owned("intro", Some("looks")),
                folder("show", None),
                owned("looks", None),
            ],
            ..Collection::default()
        }
    }

    #[test]
    fn a_scenes_folders_come_first_under_it_and_hold_its_keyframes_and_sequences() {
        let c = scene_folder_collection();
        let all = |_: &str, _: bool, _: &[LabelId]| true;
        let none = HashSet::new();
        assert_eq!(
            shown(&rows(&c, &none, &HashSet::new(), false, &all)),
            [
                "SHOW/@0",
                "A@1",
                "B@0",
                "LOOKS/@1",
                "INTRO/@2",
                "K1@3",
                "~Lights up@3",
                "K2@1",
                "~Fade@1",
            ]
        );
        // A shut folder hides what it holds; the scene's caret folds them all away.
        assert_eq!(
            shown(&rows(
                &c,
                &HashSet::from([FolderId::new("looks")]),
                &HashSet::new(),
                false,
                &all
            )),
            ["SHOW/@0", "A@1", "B@0", "LOOKS/-@1", "K2@1", "~Fade@1"]
        );
        assert_eq!(
            shown(&rows(
                &c,
                &none,
                &HashSet::from([SceneId::new("b")]),
                false,
                &all
            )),
            ["SHOW/@0", "A@1", "B@0"]
        );
        // A filter keeps the folders of what matches.
        let lights = |name: &str, _: bool, _: &[LabelId]| name == "Lights up";
        assert_eq!(
            shown(&rows(&c, &none, &HashSet::new(), true, &lights)),
            ["LOOKS/+@1", "INTRO/+@2", "~Lights up@3"]
        );
        assert_eq!(
            scene_folders(&c, &SceneId::new("b")),
            [
                (FolderId::new("looks"), "LOOKS".to_owned()),
                (FolderId::new("intro"), "LOOKS / INTRO".to_owned()),
            ]
        );
    }

    #[test]
    fn keyframes_sequences_and_a_scenes_folders_move_only_under_their_scene() {
        let c = scene_folder_collection();
        let (k1, k2) = (scene_node("k1"), scene_node("k2"));
        let (fade, lights) = (
            Node::Sequence("fade".into()),
            Node::Sequence("lights_up".into()),
        );
        let (looks, intro, show) = (
            folder_node("looks"),
            folder_node("intro"),
            folder_node("show"),
        );
        // A keyframe dropped into one of its scene's folders goes in it, and beside a
        // row there joins that row's folder.
        assert_eq!(
            moved(&c, &k2, &looks, Side::Into),
            [Edit::SceneSetFolder {
                scene_id: "k2".into(),
                folder_id: Some("looks".into()),
            }]
        );
        assert_eq!(
            moved(&c, &k2, &lights, Side::After),
            [Edit::SceneSetFolder {
                scene_id: "k2".into(),
                folder_id: Some("intro".into()),
            }]
        );
        // A sequence likewise, as its whole timeline with the new folder.
        let [Edit::TimelineSet { scene_id, timeline }]: [Edit; 1] =
            moved(&c, &fade, &intro, Side::Into)
                .try_into()
                .expect("one edit")
        else {
            panic!("a timeline.set");
        };
        assert_eq!(scene_id, SceneId::new("b"));
        assert_eq!(timeline.folder, Some(FolderId::new("intro")));
        assert_eq!(timeline.name, "Fade");
        // Out of a folder: beside an unfiled row.
        let [Edit::TimelineSet { timeline, .. }]: [Edit; 1] = moved(&c, &lights, &k2, Side::Before)
            .try_into()
            .expect("one edit")
        else {
            panic!("a timeline.set");
        };
        assert_eq!(timeline.folder, None);
        // A scene's folder moves among its scene's folders.
        assert_eq!(
            moved(&c, &intro, &k2, Side::After),
            [Edit::FolderSetParent {
                folder_id: "intro".into(),
                parent: None,
            }]
        );
        // Nothing crosses between the scene list and a scene's rows.
        assert_eq!(moved(&c, &k1, &show, Side::Into), []);
        assert_eq!(moved(&c, &scene_node("a"), &looks, Side::Into), []);
        assert_eq!(moved(&c, &looks, &show, Side::Into), []);
        assert_eq!(moved(&c, &fade, &scene_node("a"), Side::After), []);
        assert_eq!(moved(&c, &show, &k2, Side::After), []);
        // Nothing for a drop where it already is.
        assert_eq!(moved(&c, &k1, &intro, Side::Into), []);
    }

    #[test]
    fn a_mouse_picks_a_scene_on_the_press_and_a_touch_or_pen_on_the_click() {
        let none = Modifiers::default();
        assert!(picks_on_press("mouse", 0, none));
        // The other buttons.
        assert!(!picks_on_press("mouse", 1, none));
        assert!(!picks_on_press("mouse", 2, none));
        // Shift, which the reorder leaves alone, and Ctrl and Meta: on macOS a Ctrl+click
        // opens the row menu and must not switch program.
        let shift = Modifiers {
            shift: true,
            ..none
        };
        let ctrl = Modifiers { ctrl: true, ..none };
        let meta = Modifiers { meta: true, ..none };
        assert!(!picks_on_press("mouse", 0, shift));
        assert!(!picks_on_press("mouse", 0, ctrl));
        assert!(!picks_on_press("mouse", 0, meta));
        // A press that may be a scroll.
        assert!(!picks_on_press("touch", 0, none));
        assert!(!picks_on_press("pen", 0, none));
        assert!(!picks_on_press("", 0, none));
    }
}
