//! The Scene objects panel (spec-panels.md 2): the selected scene's items, top of the
//! stack first, under the `In “<scene>”, top to bottom` caption and the search box and
//! favourites and labels filter. Each row shows its source's type icon and name (in
//! italics when the source is placed in other scenes too), the chips of its labels, a
//! visibility eye, the labels popover and a favourite star. The label manager opens over
//! the rows (`labels.rs`).
//! A click selects an item (Shift adds or takes it out), a double click opens its
//! Properties dialog, F2 renames its source in place and Del removes it from the scene;
//! rows are reordered by dragging them (`item.reorder`). A nested scene's row has a caret
//! that shows its items under it (OB-04): a click on one of them opens that scene with
//! it selected, and a drop on the nested scene's row, or beside one of its items, moves
//! the dragged item into it (`item.move`). A right-click on a row opens the
//! selected items' menu (`item_menu.rs`), plus deleting the source everywhere through
//! `delete.plan`, and one on the panel's empty space opens the panel's menu, whose Add
//! source opens the Add source dialog (`add_source.rs`).
//!
//! A keyframe scene lists its parent's items as it shows them (ADR-0049): the eye
//! writes the keyframe's override, and what would change the parent's items (Del,
//! dragging, Add source) is off, as the caption says.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#objects

use crate::app::SessionContext;
use crate::icons::Icon;
use crate::item_menu;
use crate::menu::{Entry, Menus};
use crate::panels::add_source::{AddSourceDialog, type_label};
use crate::panels::confirm::{Confirm, Pending};
use crate::panels::inspector::is_shared;
use crate::panels::labels::{LabelChips, LabelEditor, LabelManager, LabelPicker, carried};
use crate::panels::scenes::RenameField;
use crate::panels::tree::{Filter, Reorder, SearchBar, Side, Star, dropped_at, indent};
use crate::store::{Collection, Studio, apply, plan_delete, plan_summary};
use jstream_control::protocol::{
    DeleteTarget, Edit, ItemId, LabelId, SceneId, SourceId, SourceKind,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::HashSet;

/// One row of the list, as the view needs it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Row {
    item_id: ItemId,
    source_id: SourceId,
    name: String,
    /// The source's type icon.
    icon: &'static str,
    visible: bool,
    /// Whether the source is placed in other scenes too.
    shared: bool,
    favourite: bool,
    /// The scene the item is in: the selected one, or a scene nested in it.
    scene_id: SceneId,
    /// How deep it is nested under the selected scene's own items.
    depth: usize,
    /// The scene the item nests, when that has items to show under it.
    nests: Option<SceneId>,
    /// Whether its items show under it.
    open: bool,
    /// Whether a search keeps it open, for a match inside it.
    kept: bool,
    /// The labels the source carries, for the filter. The row's chips follow the
    /// collection themselves, so a label put on doesn't rebuild the row and close its
    /// popover.
    label_ids: Vec<LabelId>,
}

impl Row {
    /// What rebuilds the row when it changes: everything but its labels.
    fn key(&self) -> Row {
        Row {
            label_ids: Vec::new(),
            ..self.clone()
        }
    }
}

/// The icon of a source of this kind: its type's, the scene icon for a nested scene.
pub(crate) fn icon_of(kind: Option<&SourceKind>) -> &'static str {
    match kind {
        Some(SourceKind::Builtin { kind }) => type_label(kind).map_or("image", |(_, icon)| icon),
        Some(SourceKind::Scene { .. }) => "layers",
        _ => "image",
    }
}

/// The selected scene's items, top of the stack first, each open nested scene's items
/// under it. `shows` says whether a row of this name, a favourite or not, carrying these
/// labels, passes the filters; while `filtering`, a nested scene shows when it or anything inside it does,
/// and opens to show its matches.
fn rows(
    collection: &Collection,
    scene_id: &SceneId,
    open: &HashSet<ItemId>,
    filtering: bool,
    shows: &dyn Fn(&str, bool, &[LabelId]) -> bool,
) -> Vec<Row> {
    let mut out = Vec::new();
    let mut walk = Walk {
        collection,
        open,
        filtering,
        shows,
        seen: vec![scene_id.clone()],
    };
    walk.level(scene_id, 0, &mut out);
    out
}

/// The walk down the nested scenes that [`rows`] makes.
struct Walk<'a> {
    collection: &'a Collection,
    open: &'a HashSet<ItemId>,
    filtering: bool,
    shows: &'a dyn Fn(&str, bool, &[LabelId]) -> bool,
    /// The scenes on the way down, which a nested scene can't repeat.
    seen: Vec<SceneId>,
}

impl Walk<'_> {
    /// Adds `scene_id`'s rows at `depth` to `out`.
    fn level(&mut self, scene_id: &SceneId, depth: usize, out: &mut Vec<Row>) {
        let collection = self.collection;
        let Some(scene) = collection.scene(scene_id) else {
            return;
        };
        for item in scene.items.iter().rev() {
            let source = collection.source(&item.source_id);
            let nests = match source.map(|s| &s.kind) {
                Some(SourceKind::Scene { scene_id: nested })
                    if !self.seen.contains(nested)
                        && collection
                            .scene(nested)
                            .is_some_and(|s| !s.items.is_empty()) =>
                {
                    Some(nested.clone())
                }
                _ => None,
            };
            let mut inside = Vec::new();
            if let Some(nested) = &nests {
                self.seen.push(nested.clone());
                self.level(nested, depth + 1, &mut inside);
                self.seen.pop();
            }
            let name = collection.source_name(&item.source_id);
            let favourite = source.is_some_and(|s| s.meta.favourite);
            let label_ids = source.map(|s| s.meta.labels.clone()).unwrap_or_default();
            let kept = self.filtering && !inside.is_empty();
            if self.filtering && !kept && !(self.shows)(&name, favourite, &label_ids) {
                continue;
            }
            let open = kept || (nests.is_some() && self.open.contains(&item.item_id));
            out.push(Row {
                item_id: item.item_id.clone(),
                source_id: item.source_id.clone(),
                name,
                icon: icon_of(source.map(|s| &s.kind)),
                visible: item.visible,
                shared: is_shared(collection, item),
                favourite,
                scene_id: scene_id.clone(),
                depth,
                nests,
                open,
                kept,
                label_ids,
            });
            if open {
                out.append(&mut inside);
            }
        }
    }
}

/// The sources list.
#[component]
pub fn Sources() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    // The row being renamed, by item: a source placed twice is renamed in one row.
    let renaming = RwSignal::new(None::<ItemId>);
    let pending = RwSignal::new(None::<Pending>);
    let menus = use_context::<Menus>();
    let adder = use_context::<AddSourceDialog>();
    let item_cx = item_menu::Context::from_page();
    let filter = Filter::new();
    let reorder = Reorder::new();
    // The nested scenes' rows that show their items, by item.
    let expanded = RwSignal::new(HashSet::<ItemId>::new());
    let manager = use_context::<LabelManager>();

    let scene = move || studio.selection.with(|s| s.scene.clone());
    // The selected scene's parent's name, when it is a keyframe scene.
    let keyframe_of = Memo::new(move |_| {
        let scene_id = scene()?;
        studio
            .collection
            .with(|c| c.as_ref()?.parent_of(&scene_id).map(|p| p.name.clone()))
    });
    let in_keyframe = move || keyframe_of.with(Option::is_some);
    let scene_name = move || {
        let scene_id = scene()?;
        studio
            .collection
            .with(|c| c.as_ref()?.scene(&scene_id).map(|s| s.name.clone()))
    };
    // The selected scene's own items, top first.
    let all_rows = move || {
        let Some(scene_id) = scene() else {
            return Vec::new();
        };
        let none = HashSet::new();
        studio.collection.with(|c| {
            c.as_ref()
                .map(|c| rows(c, &scene_id, &none, false, &|_, _, _| true))
                .unwrap_or_default()
        })
    };
    let shown_rows = move || {
        let Some(scene_id) = scene() else {
            return Vec::new();
        };
        let filtering = filter.active();
        let shows =
            |name: &str, favourite: bool, labels: &[LabelId]| filter.shows(name, favourite, labels);
        expanded.with(|open| {
            studio.collection.with(|c| {
                c.as_ref()
                    .map(|c| rows(c, &scene_id, open, filtering, &shows))
                    .unwrap_or_default()
            })
        })
    };

    let ask_delete = move |source_id: SourceId, name: String| {
        let Some(s) = session.get() else { return };
        spawn_local(async move {
            match plan_delete(&s, DeleteTarget::Source { source_id }).await {
                Ok((rev, edits)) => pending.set(Some(Pending {
                    question: format!("Delete source “{name}” from every scene?"),
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
    let send = move |edit: Edit| {
        if let Some(s) = session.get() {
            studio.apply(&s, vec![edit]);
        }
    };
    // The list is top first and `item.reorder` counts from the bottom, so a drop above a
    // row goes after it in the scene's order.
    let drop = move |moved: String, target: String, side: Side| {
        let Some(scene_id) = scene() else { return };
        // A row dropped on itself, or into its own nested scene, is no move.
        if moved == target {
            return;
        }
        // Into a nested scene, or beside one of its items: an `item.move` there.
        let into = studio.collection.with_untracked(|c| {
            let c = c.as_ref()?;
            let (target_scene, item) = c.item(&ItemId::new(target.clone()))?;
            if side == Side::Into {
                let SourceKind::Scene { scene_id: nested } = &c.source(&item.source_id)?.kind
                else {
                    return None;
                };
                let top = c.scene(nested)?.items.len();
                return Some((nested.clone(), top));
            }
            if target_scene.scene_id == scene_id {
                return None;
            }
            let at = target_scene
                .items
                .iter()
                .position(|i| i.item_id == item.item_id)?;
            // The list is top first: above a row is one place higher in the stack.
            let index = if side == Side::Before { at + 1 } else { at };
            Some((target_scene.scene_id.clone(), index))
        });
        if let Some((into, index)) = into {
            send(Edit::ItemMove {
                item_id: ItemId::new(moved),
                scene_id: into,
                index: u32::try_from(index).unwrap_or(u32::MAX),
            });
            return;
        }
        if side == Side::Into {
            return;
        }
        let index = studio.collection.with_untracked(|c| {
            let order: Vec<&str> = c
                .as_ref()?
                .scene(&scene_id)?
                .items
                .iter()
                .map(|i| i.item_id.as_str())
                .collect();
            let side = match side {
                Side::Before => Side::After,
                Side::After => Side::Before,
                Side::Into => Side::Into,
            };
            dropped_at(&order, &moved.as_str(), &target.as_str(), side)
        });
        if let Some(index) = index {
            send(Edit::ItemReorder {
                item_id: ItemId::new(moved),
                index,
            });
        }
    };

    let row = move |row: Row| {
        let Row {
            item_id,
            source_id,
            name,
            icon,
            visible,
            shared,
            favourite,
            scene_id: row_scene,
            depth,
            nests,
            open,
            kept,
            ..
        } = row;
        let labels = {
            let id = source_id.clone();
            Signal::derive(move || {
                studio.collection.with(|c| {
                    c.as_ref()
                        .and_then(|c| Some(carried(c, &c.source(&id)?.meta.labels)))
                        .unwrap_or_default()
                })
            })
        };
        let key = item_id.to_string();
        // A nested scene's item is selected in its own scene, which the panel then shows.
        let inside = depth > 0;
        let select_here = {
            let scene = row_scene.clone();
            move |id: ItemId| {
                if inside {
                    studio.select_scene(scene.clone());
                }
                studio.select_item(Some(id));
            }
        };
        let selected = {
            let id = item_id.clone();
            move || studio.selection.with(|s| s.contains(&id))
        };
        let is_renaming = {
            let id = item_id.clone();
            move || renaming.with(|r| r.as_ref() == Some(&id))
        };
        // A click selects the item alone; Shift-click adds it to the selection or takes
        // it out, as on the canvas.
        let select = {
            let id = item_id.clone();
            let select_here = select_here.clone();
            move |event: leptos::ev::MouseEvent| {
                if event.shift_key() && !inside {
                    studio.toggle_item(&id);
                } else {
                    select_here(id.clone());
                }
            }
        };
        let open_properties = {
            let id = item_id.clone();
            let select_here = select_here.clone();
            move |_| {
                if let Some(cx) = item_cx {
                    select_here(id.clone());
                    cx.properties(id.clone());
                }
            }
        };
        let rename = {
            let id = source_id.clone();
            let old = name.clone();
            move |to: String| {
                renaming.set(None);
                let to = to.trim().to_owned();
                if !to.is_empty() && to != old {
                    send(Edit::SourceRename {
                        source_id: id.clone(),
                        name: to,
                    });
                }
            }
        };
        // F2 renames the source, Del removes the item from the scene, and Ctrl+D and
        // Ctrl+Shift+D duplicate it as a reference or a clone, as the menu's hints say. In a
        // keyframe, Del and the duplicates do nothing, and run no hotkey either.
        let on_key = {
            let item = item_id.clone();
            let scene_id = row_scene.clone();
            move |event: leptos::ev::KeyboardEvent| match event.key().as_str() {
                "d" | "D" if event.ctrl_key() || event.meta_key() => {
                    event.prevent_default();
                    if let Some(cx) = item_cx.filter(|_| !in_keyframe()) {
                        cx.duplicate_selected(
                            &scene_id,
                            std::slice::from_ref(&item),
                            event.shift_key(),
                        );
                    }
                }
                "F2" => {
                    event.prevent_default();
                    renaming.set(Some(item.clone()));
                }
                "Delete" => {
                    event.prevent_default();
                    if !in_keyframe() {
                        send(Edit::ItemRemove {
                            item_id: item.clone(),
                        });
                    }
                }
                _ => {}
            }
        };
        let toggle = {
            let item = item_id.clone();
            move |_: leptos::ev::MouseEvent| {
                // A nested scene's item belongs to that scene, which a keyframe doesn't
                // override.
                if inside && in_keyframe() {
                    return;
                }
                // Addressed to the selected scene: in a keyframe, its override.
                send(Edit::ItemSetVisible {
                    item_id: item.clone(),
                    scene_id: if inside { None } else { scene() },
                    visible: !visible,
                });
            }
        };
        let toggle_favourite = {
            let id = source_id.clone();
            move || {
                let edit = studio.collection.with_untracked(|c| {
                    let source = c.as_ref()?.source(&id)?;
                    let mut meta = source.meta.clone();
                    meta.favourite = !meta.favourite;
                    Some(Edit::SourceSetMeta {
                        source_id: id.clone(),
                        meta,
                    })
                });
                if let Some(edit) = edit {
                    send(edit);
                }
            }
        };
        // A right-click on the row opens the selected items' menu, after selecting the
        // item alone unless it is already selected. With one item, deleting its source
        // everywhere follows.
        let on_row_context = {
            let id = item_id.clone();
            let (source, name) = (source_id.clone(), name.clone());
            let scene_id = row_scene.clone();
            let select_here = select_here.clone();
            move |event: leptos::ev::MouseEvent| {
                let (Some(menus), Some(cx)) = (menus, item_cx) else {
                    return;
                };
                if inside || !studio.selection.with_untracked(|s| s.contains(&id)) {
                    select_here(id.clone());
                }
                let items: Vec<ItemId> = studio
                    .selection
                    .with_untracked(|s| s.items().cloned().collect());
                let mut entries =
                    item_menu::entries(cx, &scene_id, &items, item_menu::Scope::Objects);
                if entries.is_empty() {
                    return;
                }
                if items.len() == 1 {
                    let (source, name) = (source.clone(), name.clone());
                    entries.extend([
                        Entry::Sep,
                        Entry::item("Delete source everywhere…", move || {
                            ask_delete(source.clone(), name.clone());
                        }),
                    ]);
                }
                menus.open(&event, entries);
            }
        };
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
            move |event: leptos::ev::PointerEvent| {
                if !inside
                    && !in_keyframe()
                    && !filter.active()
                    && renaming.with_untracked(Option::is_none)
                {
                    reorder.down(&key, &event);
                }
            }
        };
        let shown = name.clone();
        let eye_label = format!("Show or hide {name}");
        let star_name = name.clone();
        let picker_name = name.clone();
        let picker_source = source_id.clone();
        let container = nests.is_some();
        let caret = nests.map(|_| {
            let id = item_id.clone();
            let label = format!("{} {name}", if open { "Collapse" } else { "Expand" });
            let toggle = move |_| {
                expanded.update(|open| {
                    if !open.remove(&id) {
                        open.insert(id.clone());
                    }
                });
            };
            view! {
                <button
                    type="button"
                    class="tree-caret"
                    aria-label=label
                    aria-expanded=open.to_string()
                    disabled=kept
                    on:click=toggle
                >
                    {if open { "▾" } else { "▸" }}
                </button>
            }
        });
        view! {
            <li
                class="row tree-row"
                class:selected=selected
                class:hidden=!visible
                class:inside=inside
                class:drop-before=drop_side(Side::Before)
                class:drop-after=drop_side(Side::After)
                class:drop-into=drop_side(Side::Into)
                class:dragging=dragging
                style:padding-left=indent(depth)
                data-tree-id=key
                data-tree-container=container.then_some("true")
                on:contextmenu=on_row_context
                on:keydown=on_key
                on:pointerdown=press
            >
                {match caret {
                    Some(caret) => caret.into_any(),
                    None => view! { <span class="tree-caret"></span> }.into_any(),
                }}
                <Show
                    when=is_renaming
                    fallback=move || {
                        let name = shown.clone();
                        view! {
                            <button class="row-main" on:click=select.clone() on:dblclick=open_properties.clone()>
                                <Icon name=icon size=15 />
                                <span class="tree-label" class:reference=shared>{name}</span>
                            </button>
                        }
                    }
                >
                    <RenameField value=name.clone() done=rename.clone() />
                </Show>
                <LabelChips labels=labels />
                <button
                    type="button"
                    class="hov tree-tool eye"
                    class:on=!visible
                    title="Visible in this scene"
                    aria-label=eye_label
                    aria-pressed=(!visible).to_string()
                    on:click=toggle
                >
                    <Icon name=if visible { "eye" } else { "eye-off" } size=14 />
                </button>
                <LabelPicker source_id=picker_source name=picker_name />
                <Star name=star_name on=Signal::stored(favourite) toggle=toggle_favourite />
            </li>
        }
    };

    // A right-click in the panel opens its menu (spec-workspace.md 5.2.4).
    let on_context = move |event: leptos::ev::MouseEvent| {
        let (Some(menus), Some(scene_id)) = (menus, scene()) else {
            return;
        };
        let group_into = scene_id.clone();
        let open = move |scene_type: bool| {
            let scene_id = scene_id.clone();
            move || {
                if let Some(adder) = adder {
                    adder.open(scene_id.clone(), scene_type);
                }
            }
        };
        let select_all = move || {
            let items: Vec<ItemId> = all_rows().into_iter().rev().map(|r| r.item_id).collect();
            studio.select_items(items);
        };
        let fixed = adder.is_none() || in_keyframe();
        menus.open(
            &event,
            vec![
                Entry::item("Add source…", open(false)).disabled_if(fixed),
                Entry::item("Add scene as source…", open(true)).disabled_if(fixed),
                {
                    Entry::item("New group", move || {
                        if let Some(cx) = item_cx {
                            let items: Vec<ItemId> = studio
                                .selection
                                .with_untracked(|s| s.items().cloned().collect());
                            cx.group(&group_into, &items);
                        }
                    })
                    .disabled_if(item_cx.is_none() || in_keyframe())
                },
                Entry::Sep,
                Entry::item("Select all", select_all).hint("Ctrl+A"),
                Entry::unavailable("Paste").hint("Ctrl+V"),
            ],
        );
    };

    view! {
        <section class="panel sources-panel tree-panel" on:contextmenu=on_context>
            <h2>"Sources"</h2>
            <SearchBar what="objects" filter=filter labels=true />
            {move || manager.is_some_and(|m| m.shown.get()).then(|| view! { <LabelEditor /> })}
            <Confirm pending=pending confirm=confirm_delete />
            <Show
                when=move || scene().is_some()
                fallback=|| view! { <p class="placeholder">"Select a scene."</p> }
            >
                <p class="tree-caption">
                    "In “" {scene_name} "”, top to bottom"
                </p>
                {move || keyframe_of.get().map(|parent| view! {
                    <p class="tree-caption keyframe-note">
                        "Keyframe of “" {parent} "”: its objects. Add, remove and restack them there."
                    </p>
                })}
                <ul
                    class="rows tree-rows"
                    class:reordering=move || reorder.dragged.with(Option::is_some)
                    on:pointermove=move |event| reorder.moved(&event)
                    on:pointerup=move |_| reorder.up(drop)
                    on:pointercancel=move |_| reorder.cancel()
                >
                    <For each=shown_rows key=Row::key children=row />
                </ul>
                {move || {
                    let none = all_rows().is_empty();
                    if none {
                        Some(view! { <p class="placeholder">"This scene is empty."</p> }.into_any())
                    } else if shown_rows().is_empty() {
                        Some(view! { <p class="placeholder">"Nothing matches these filters."</p> }.into_any())
                    } else {
                        None
                    }
                }}
            </Show>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::rows;
    use crate::store::Collection;
    use jstream_control::protocol::ItemId;
    use jstream_control::protocol::{
        Canvas, Crop, Finite32, ItemInfo, ItemTransform, PlacementForm, SceneId, SceneInfo,
        SceneRole, SourceInfo, SourceKind,
    };
    use std::collections::HashSet;

    fn collection() -> Collection {
        let item = |id: &str, source: &str, visible: bool| ItemInfo {
            item_id: id.into(),
            source_id: source.into(),
            placement: PlacementForm::Legacy,
            transform: ItemTransform::IDENTITY,
            geometry: None,
            crop: Crop::NONE,
            visible,
            effects: Vec::new(),
            opacity: Finite32::ONE,
            blend: Default::default(),
            transition: jstream_control::protocol::ItemTransition::default(),
        };
        Collection {
            canvas: Canvas {
                width: 1280,
                height: 720,
            },
            sources: vec![SourceInfo {
                source_id: "cam".into(),
                name: "Camera".into(),
                kind: SourceKind::Builtin {
                    kind: "webcam".into(),
                },
                settings: Default::default(),
                duplicated_from: None,
                audio: None,
                filters: Default::default(),
                meta: Default::default(),
            }],
            scenes: vec![SceneInfo {
                overrides: Vec::new(),
                filters: Default::default(),
                variant_of: None,
                scene_id: "main".into(),
                name: "Main".into(),
                role: SceneRole::Scene,
                folder: None,
                meta: Default::default(),
                items: vec![item("bottom", "cam", true), item("top", "gone", false)],
                transition_in: None,
                transition_out: None,
            }],
            ..Collection::default()
        }
    }

    fn all(c: &Collection, scene: &str) -> Vec<super::Row> {
        rows(
            c,
            &SceneId::new(scene),
            &HashSet::new(),
            false,
            &|_, _, _| true,
        )
    }

    /// `main` holds the camera and a nested `grp`, which holds a logo and a nested
    /// `inner` holding a title; `inner` also nests `grp`, which the walk doesn't repeat.
    fn nested() -> Collection {
        let mut c = collection();
        let scene = |id: &str, items: Vec<ItemInfo>| SceneInfo {
            scene_id: id.into(),
            name: id.into(),
            items,
            ..c.scenes[0].clone()
        };
        let source = |id: &str, kind: SourceKind| SourceInfo {
            source_id: id.into(),
            name: id.to_uppercase(),
            kind,
            ..c.sources[0].clone()
        };
        let shows = |scene: &str| SourceKind::Scene {
            scene_id: scene.into(),
        };
        let item = |id: &str, source: &str| ItemInfo {
            item_id: id.into(),
            source_id: source.into(),
            ..c.scenes[0].items[0].clone()
        };
        c.sources.extend([
            source("grp_src", shows("grp")),
            source("inner_src", shows("inner")),
            source(
                "logo",
                SourceKind::Builtin {
                    kind: "image".into(),
                },
            ),
            source(
                "title",
                SourceKind::Builtin {
                    kind: "text".into(),
                },
            ),
        ]);
        c.scenes = vec![
            scene("main", vec![item("cam", "cam"), item("g", "grp_src")]),
            scene("grp", vec![item("logo1", "logo"), item("in", "inner_src")]),
            scene(
                "inner",
                vec![item("title1", "title"), item("loop", "grp_src")],
            ),
        ];
        c
    }

    fn shown(rows: &[super::Row]) -> Vec<String> {
        rows.iter()
            .map(|r| {
                let caret = match (&r.nests, r.open) {
                    (None, _) => "",
                    (Some(_), true) => "-",
                    (Some(_), false) => "+",
                };
                format!(
                    "{}{}{caret}@{}",
                    "  ".repeat(r.depth),
                    r.item_id,
                    r.scene_id
                )
            })
            .collect()
    }

    #[test]
    fn a_nested_scene_opens_to_show_its_items() {
        let c = nested();
        assert_eq!(shown(&all(&c, "main")), ["g+@main", "cam@main"]);
        let open: HashSet<ItemId> = [ItemId::new("g"), ItemId::new("in")].into();
        let rows = rows(&c, &SceneId::new("main"), &open, false, &|_, _, _| true);
        assert_eq!(
            shown(&rows),
            [
                "g-@main",
                "  in-@grp",
                "    loop@inner",
                "    title1@inner",
                "  logo1@grp",
                "cam@main",
            ]
        );
    }

    #[test]
    fn a_search_keeps_the_nested_scenes_that_hold_a_match_open() {
        let c = nested();
        let rows = rows(
            &c,
            &SceneId::new("main"),
            &HashSet::new(),
            true,
            &|name, _, _| name == "TITLE",
        );
        assert_eq!(shown(&rows), ["g-@main", "  in-@grp", "    title1@inner"]);
        assert!(rows[0].kept && rows[1].kept && !rows[2].kept);
    }

    #[test]
    fn a_label_inside_a_nested_scene_keeps_its_scenes_open() {
        let mut c = nested();
        let label = super::LabelId::new("lower-third");
        for source in &mut c.sources {
            if source.source_id.as_str() == "title" {
                source.meta.labels = vec![label.clone()];
            }
        }
        let rows = rows(
            &c,
            &SceneId::new("main"),
            &HashSet::new(),
            true,
            &|_, _, labels| labels.contains(&label),
        );
        assert_eq!(shown(&rows), ["g-@main", "  in-@grp", "    title1@inner"]);
        assert_eq!(rows[2].label_ids, std::slice::from_ref(&label));
        assert!(rows[0].label_ids.is_empty() && rows[0].kept);
    }

    #[test]
    fn rows_list_the_top_of_the_stack_first() {
        let rows = all(&collection(), "main");
        let ids: Vec<&str> = rows.iter().map(|r| r.item_id.as_str()).collect();
        assert_eq!(ids, ["top", "bottom"]);
        assert!(!rows[0].visible && rows[1].visible);
        assert_eq!(
            (rows[1].name.as_str(), rows[1].icon, rows[1].shared),
            ("Camera", "webcam", false)
        );
        assert_eq!(rows[0].name, "gone", "a missing source shows its ID");
        assert!(all(&collection(), "other").is_empty());
    }
}
