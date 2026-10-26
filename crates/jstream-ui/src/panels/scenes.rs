//! The Scenes panel (spec-panels.md 1): the collection's scenes in display order, under
//! a search box and a favourites filter, each row with its icon, a "program" badge on
//! the scene on program and a favourite star. Clicking a scene shows it in Scene
//! objects and the canvas and puts it on program (`scene.switch`), or, in studio mode,
//! on preview (`studio.set_preview`), where a hover button puts it on program. A double
//! click opens its Properties dialog (a keyframe's renames it). Rows are reordered by
//! dragging them (`scene.reorder`), renamed in place (F2) and deleted (Del or the menu)
//! through `delete.plan`, so the user sees what else goes. A right-click on a scene
//! opens its menu, and one on the panel's empty space opens the panel's
//! (spec-workspace.md 5.2.3 and 5.2.7).
//!
//! A scene's keyframe scenes (ADR-0024 §2) nest under it, each with the purple diamond,
//! and the scene's caret folds them away. They stay under their scene: they aren't
//! dragged, and a drop on one does nothing. A keyframe's menu switches to it, resets it
//! to its parent (`item.clear_override` for every field it overrides), makes another
//! keyframe of the parent (`scene.create_variant`) and deletes it, at once, since it
//! holds no items of its own (undo brings it back).
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#scenes

use crate::app::SessionContext;
use crate::icons::{Icon, named};
use crate::menu::{Entry, Menus};
use crate::panels::confirm::{Confirm, Pending};
use crate::panels::scene_properties::SceneDialog;
use crate::panels::studio::override_submenu;
use crate::panels::tree::{Filter, Reorder, SearchBar, Side, Star, dropped_at, indent};
use crate::store::{Collection, Studio, apply, plan_delete, plan_summary, reset_edits};
use crate::workspace::DockState;
use jstream_control::protocol::{Action, DeleteTarget, Edit, FolderId, SceneId, SceneInfo};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::HashSet;

/// The prefix that keeps a folder's row key apart from a scene's.
const FOLDER_KEY: &str = "folder:";

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
        /// Whether the scene has keyframe scenes, so its row has a caret.
        keyframes: bool,
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
}

impl Node {
    fn of(key: &str) -> Node {
        match key.strip_prefix(FOLDER_KEY) {
            Some(id) => Node::Folder(FolderId::new(id)),
            None => Node::Scene(SceneId::new(key)),
        }
    }
}

/// The folder `folder_id` sits in, if it names one that exists.
fn parent_of(collection: &Collection, folder_id: &FolderId) -> Option<FolderId> {
    collection
        .folder(folder_id)?
        .parent
        .clone()
        .filter(|p| collection.folder(p).is_some())
}

/// The folder a scene sits in, if it names one that exists.
fn folder_of(collection: &Collection, scene_id: &SceneId) -> Option<FolderId> {
    collection
        .scene(scene_id)?
        .folder
        .clone()
        .filter(|f| collection.folder(f).is_some())
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
    shows: &dyn Fn(&str, bool) -> bool,
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
    shows: &'a dyn Fn(&str, bool) -> bool,
    /// The folders already walked, so a broken parent chain can't loop.
    seen: HashSet<FolderId>,
}

impl Walk<'_> {
    /// What sits in `parent` (the top level for `None`), at `depth`.
    fn level(&mut self, parent: Option<&FolderId>, depth: usize, out: &mut Vec<Row>) {
        let collection = self.collection;
        for folder in &collection.folders {
            if parent_of(collection, &folder.folder_id).as_ref() != parent
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
            let shown: Vec<&SceneInfo> = keyframes
                .iter()
                .filter(|k| (self.shows)(&k.name, k.meta.favourite))
                .copied()
                .collect();
            if (self.shows)(&scene.name, scene.meta.favourite) {
                out.push(Row::scene(
                    scene,
                    depth,
                    collection.parent_of(&scene.scene_id),
                    !keyframes.is_empty(),
                ));
            }
            if !self.folded.contains(&scene.scene_id) {
                out.extend(
                    shown
                        .into_iter()
                        .map(|k| Row::scene(k, depth + 1, Some(scene), false)),
                );
            }
        }
    }
}

/// The edits that move the row `moved` to the `side` of the row `target` (S19), as one
/// batch: a scene or folder dropped into a folder goes in it; dropped beside a row it
/// joins that row's folder, and beside a row of its own kind it also takes the position
/// there. Nothing for a drop where it already is, or a folder dropped on itself or
/// anything within it.
fn moved(collection: &Collection, moved: &Node, target: &Node, side: Side) -> Vec<Edit> {
    // The folder the target's row sits in, or, for into, the target folder itself.
    let into = match (target, side) {
        (Node::Folder(folder), Side::Into) => Some(folder.clone()),
        (Node::Scene(_), Side::Into) => return Vec::new(),
        (Node::Folder(folder), _) => parent_of(collection, folder),
        (Node::Scene(scene), _) => folder_of(collection, scene),
    };
    let mut edits = Vec::new();
    // Keyframes stay under their scene: they aren't dragged, and a drop on one does
    // nothing.
    let keyframe = |node: &Node| matches!(node, Node::Scene(id) if collection.is_keyframe(id));
    if keyframe(moved) || keyframe(target) {
        return edits;
    }
    match moved {
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

/// The name a new keyframe scene of a scene with `count` of them gets.
fn keyframe_name(count: usize) -> String {
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
    let dock = use_context::<DockState>();
    let scene_dialog = use_context::<SceneDialog>();
    let any_rows = move || {
        studio.collection.with(|c| {
            c.as_ref()
                .is_some_and(|c| !c.folders.is_empty() || c.listed_scenes().next().is_some())
        })
    };
    let shown_rows = move || {
        let filtering = filter.active();
        let shows = |name: &str, favourite: bool| filter.shows(name, favourite, &[]);
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
    // menus' New folder).
    let new_folder = move |parent: Option<FolderId>| {
        let Some(s) = session.get() else { return };
        let folder_id = FolderId::new(studio.fresh_id(&s, "folder"));
        if let Some(parent) = &parent {
            open_folder(parent);
        }
        studio.apply(
            &s,
            vec![Edit::FolderCreate {
                folder_id: folder_id.clone(),
                name: "New folder".into(),
                parent,
            }],
        );
        renaming.set(Some(format!("{FOLDER_KEY}{folder_id}")));
    };

    let switch = move |scene_id: SceneId| {
        if let Some(s) = session.get() {
            studio.act(
                &s,
                Action::SceneSwitch {
                    scene_id,
                    preset_id: None,
                },
            );
        }
    };
    // A new keyframe scene of `parent`, unfolded and selected so it can be arranged.
    let new_keyframe = move |parent: SceneId| {
        let Some(s) = session.get() else { return };
        let count = studio
            .collection
            .with_untracked(|c| c.as_ref().map_or(0, |c| c.keyframes_of(&parent).count()));
        let scene_id = SceneId::new(studio.fresh_id(&s, "scene"));
        studio.apply(
            &s,
            vec![Edit::SceneCreateVariant {
                scene_id: scene_id.clone(),
                variant_of: parent.clone(),
                name: keyframe_name(count),
            }],
        );
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
                    && !c
                        .scenes
                        .iter()
                        .any(|sc| sc.folder.as_ref() == Some(&folder_id))
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
        // A click selects the scene, and puts it on program, or on preview in studio
        // mode.
        let select = {
            let id = scene_id.clone();
            move |_| {
                studio.select_scene(id.clone());
                if let Some(s) = session.get() {
                    studio.act(&s, studio.click_action(&id));
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
        // F2 renames and Del deletes, the keys the menu's hints name.
        let on_key = {
            let id = scene_id.clone();
            let key = key.clone();
            let name = name.clone();
            let parent = keyframe_of.clone();
            move |event: leptos::ev::KeyboardEvent| match event.key().as_str() {
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
                    let delete_name = name.clone();
                    let overrides = studio.collection.with_untracked(|c| {
                        c.as_ref()
                            .and_then(|c| c.scene(&id))
                            .is_some_and(|s| !s.overrides.is_empty())
                    });
                    menus.open(
                        &event,
                        vec![
                            Entry::item("Switch to it", move || switch(switch_id.clone())),
                            Entry::item("Rename", move || renaming.set(Some(rename_key.clone())))
                                .hint("F2"),
                            Entry::Sep,
                            Entry::submenu("Transition in", Vec::new()),
                            Entry::submenu("Transition out", Vec::new()),
                            Entry::Sep,
                            Entry::item(format!("Reset to “{parent_name}”"), move || {
                                reset_keyframe(reset_id.clone());
                            })
                            .disabled_if(!overrides),
                            Entry::item("New keyframe", move || new_keyframe(new_parent.clone())),
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
                let (delete_id, delete_name) = (id.clone(), name.clone());
                let folder = studio
                    .collection
                    .with_untracked(|c| c.as_ref().and_then(|c| folder_of(c, &id)));
                let scene_folder = folder.clone();
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
                        Entry::Sep,
                        Entry::item("Properties…", move || {
                            if let Some(dialog) = scene_dialog {
                                dialog.open(props_id.clone());
                            }
                        })
                        .hint("Double-click")
                        .disabled_if(scene_dialog.is_none()),
                        Entry::unavailable("Edit filters…"),
                        Entry::submenu("Pop out", Vec::new()),
                        Entry::Sep,
                        Entry::item("New scene", move || new_scene(scene_folder.clone())),
                        Entry::item("New keyframe", move || new_keyframe(keyframe_id.clone())),
                        Entry::item("New folder", move || new_folder(folder.clone())),
                        Entry::Sep,
                        Entry::unavailable("Duplicate scene"),
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
                on:pointerdown=move |event| {
                    if !is_keyframe {
                        press(press_key.clone())(event);
                    }
                }
            >
                <span class="tree-caret">{caret}</span>
                <Show
                    when=is_renaming
                    fallback=move || {
                        let name = shown_name.clone();
                        view! {
                            <button class="row-main" on:click=select.clone() on:dblclick=on_double_click.clone()>
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
                <Star name=star_name on=Signal::stored(favourite) toggle=toggle_favourite />
            </li>
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
                    menus.open(
                        &event,
                        vec![
                            Entry::item("New scene", move || new_scene(Some(scene_in.clone()))),
                            Entry::item("New folder", move || new_folder(Some(folder_in.clone()))),
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

    let row = move |row: Row| match row {
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
                Entry::item("New folder", move || new_folder(None)),
                Entry::Sep,
                Entry::unavailable("Paste scene").hint("Ctrl+V"),
            ],
        );
    };

    view! {
        <section class="panel scenes-panel tree-panel" on:contextmenu=on_context>
            <h2>"Scenes"</h2>
            <SearchBar what="scenes" filter=filter />
            <Confirm pending=pending confirm=confirm_delete />
            <ul
                class="rows tree-rows"
                class:reordering=move || reorder.dragged.with(Option::is_some)
                on:pointermove=move |event| reorder.moved(&event)
                on:pointerup=move |_| reorder.up(drop)
                on:pointercancel=move |_| reorder.cancel()
            >
                <For each=shown_rows key=|row| row.clone() children=row />
            </ul>
            {move || {
                let none = !any_rows();
                let hidden = !none && shown_rows().is_empty();
                (studio.collection.with(Option::is_some) && none)
                    .then(|| view! { <p class="placeholder">"No scenes yet."</p> }.into_any())
                    .or_else(|| {
                        hidden.then(|| view! { <p class="placeholder">"Nothing matches these filters."</p> }.into_any())
                    })
            }}
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

#[cfg(test)]
mod tests {
    use super::{Node, Row, keyframe_name, moved, rows};
    use crate::panels::tree::Side;
    use crate::store::Collection;
    use jstream_control::protocol::{Edit, FolderId, FolderInfo, SceneId, SceneInfo, SceneRole};
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
            variant_of: None,
            overrides: Vec::new(),
            items: Vec::new(),
            filters: Default::default(),
        }
    }

    fn folder(id: &str, parent: Option<&str>) -> FolderInfo {
        FolderInfo {
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
            })
            .collect()
    }

    #[test]
    fn folders_come_first_at_each_level_and_hold_their_scenes() {
        let all = |_: &str, _: bool| true;
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
        let b_only = |name: &str, _: bool| name == "B";
        assert_eq!(
            shown(&rows(&c, &shut, &HashSet::new(), true, &b_only)),
            ["SHOW/+@0", "OVERLAYS/+@1", "B@2"]
        );
        let favourites = |_: &str, favourite: bool| favourite;
        assert_eq!(
            shown(&rows(&c, &shut, &HashSet::new(), true, &favourites)),
            ["SHOW/+@0", "A@1"]
        );
        let nothing = |_: &str, _: bool| false;
        assert!(rows(&c, &shut, &HashSet::new(), true, &nothing).is_empty());
    }

    #[test]
    fn a_parent_loop_shows_nothing_twice() {
        let mut c = collection();
        c.folders = vec![folder("x", Some("y")), folder("y", Some("x"))];
        let all = |_: &str, _: bool| true;
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
        let all = |_: &str, _: bool| true;
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
        let k1_only = |name: &str, _: bool| name == "K1";
        assert_eq!(
            shown(&rows(&c, &none, &HashSet::new(), true, &k1_only)),
            ["K1@1"]
        );
        // Keyframes don't move, and nothing lands on one.
        assert!(moved(&c, &scene_node("k1"), &scene_node("a"), Side::Before).is_empty());
        assert!(moved(&c, &scene_node("a"), &scene_node("k1"), Side::After).is_empty());
        assert_eq!(keyframe_name(2), "Keyframe 3");
    }
}
