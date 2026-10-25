//! The Scene objects panel (spec-panels.md 2): the selected scene's items, top of the
//! stack first, under the `In “<scene>”, top to bottom` caption and the search box and
//! favourites and labels filter. Each row shows its source's type icon and name (in
//! italics when the source is placed in other scenes too), the chips of its labels, a
//! visibility eye, the labels popover and a favourite star. The label manager opens over
//! the rows (`labels.rs`).
//! A click selects an item (Shift adds or takes it out), a double click opens its
//! Properties dialog, F2 renames its source in place and Del removes it from the scene;
//! rows are reordered by dragging them (`item.reorder`). A right-click on a row opens the
//! selected items' menu (`item_menu.rs`), plus deleting the source everywhere through
//! `delete.plan`, and one on the panel's empty space opens the panel's menu, whose Add
//! source opens the Add source dialog (`add_source.rs`).
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
use crate::panels::tree::{Filter, Reorder, SearchBar, Side, Star, dropped_at};
use crate::store::{Collection, Studio, apply, plan_delete, plan_summary};
use jstream_control::protocol::{
    DeleteTarget, Edit, ItemId, LabelId, SceneId, SourceId, SourceKind,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

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
    /// The labels the source carries, for the filter. The row's chips follow the
    /// collection themselves, so a label put on doesn't rebuild the row and close its
    /// popover.
    label_ids: Vec<LabelId>,
}

impl Row {
    /// What rebuilds the row when it changes: everything but its labels.
    fn key(&self) -> (ItemId, String, &'static str, bool, bool, bool) {
        (
            self.item_id.clone(),
            self.name.clone(),
            self.icon,
            self.visible,
            self.shared,
            self.favourite,
        )
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

/// The selected scene's items, top of the stack first.
fn rows(collection: &Collection, scene_id: &SceneId) -> Vec<Row> {
    let Some(scene) = collection.scene(scene_id) else {
        return Vec::new();
    };
    scene
        .items
        .iter()
        .rev()
        .map(|item| {
            let source = collection.source(&item.source_id);
            Row {
                item_id: item.item_id.clone(),
                source_id: item.source_id.clone(),
                name: collection.source_name(&item.source_id),
                icon: icon_of(source.map(|s| &s.kind)),
                visible: item.visible,
                shared: is_shared(collection, item),
                favourite: source.is_some_and(|s| s.meta.favourite),
                label_ids: source.map(|s| s.meta.labels.clone()).unwrap_or_default(),
            }
        })
        .collect()
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
    let manager = use_context::<LabelManager>();

    let scene = move || studio.selection.with(|s| s.scene.clone());
    let scene_name = move || {
        let scene_id = scene()?;
        studio
            .collection
            .with(|c| c.as_ref()?.scene(&scene_id).map(|s| s.name.clone()))
    };
    let all_rows = move || {
        let Some(scene_id) = scene() else {
            return Vec::new();
        };
        studio
            .collection
            .with(|c| c.as_ref().map(|c| rows(c, &scene_id)).unwrap_or_default())
    };
    let shown_rows = move || {
        all_rows()
            .into_iter()
            .filter(|row| filter.shows(&row.name, row.favourite, &row.label_ids))
            .collect::<Vec<_>>()
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
            move |event: leptos::ev::MouseEvent| {
                if event.shift_key() {
                    studio.toggle_item(&id);
                } else {
                    studio.select_item(Some(id.clone()));
                }
            }
        };
        let open_properties = {
            let id = item_id.clone();
            move |_| {
                if let Some(cx) = item_cx {
                    studio.select_item(Some(id.clone()));
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
        // F2 renames the source, Del removes the item from the scene, as the menu's
        // hints say.
        let on_key = {
            let item = item_id.clone();
            move |event: leptos::ev::KeyboardEvent| match event.key().as_str() {
                "F2" => {
                    event.prevent_default();
                    renaming.set(Some(item.clone()));
                }
                "Delete" => {
                    event.prevent_default();
                    send(Edit::ItemRemove {
                        item_id: item.clone(),
                    });
                }
                _ => {}
            }
        };
        let toggle = {
            let item = item_id.clone();
            move |_: leptos::ev::MouseEvent| {
                send(Edit::ItemSetVisible {
                    item_id: item.clone(),
                    scene_id: None,
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
            move |event: leptos::ev::MouseEvent| {
                let (Some(menus), Some(cx), Some(scene_id)) = (menus, item_cx, scene()) else {
                    return;
                };
                if !studio.selection.with_untracked(|s| s.contains(&id)) {
                    studio.select_item(Some(id.clone()));
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
                if !filter.active() && renaming.with_untracked(Option::is_none) {
                    reorder.down(&key, &event);
                }
            }
        };
        let shown = name.clone();
        let eye_label = format!("Show or hide {name}");
        let star_name = name.clone();
        let picker_name = name.clone();
        let picker_source = source_id.clone();
        view! {
            <li
                class="row tree-row"
                class:selected=selected
                class:hidden=!visible
                class:drop-before=drop_side(Side::Before)
                class:drop-after=drop_side(Side::After)
                class:dragging=dragging
                data-tree-id=key
                on:contextmenu=on_row_context
                on:keydown=on_key
                on:pointerdown=press
            >
                <span class="tree-caret"></span>
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
        menus.open(
            &event,
            vec![
                Entry::item("Add source…", open(false)).disabled_if(adder.is_none()),
                Entry::item("Add scene as source…", open(true)).disabled_if(adder.is_none()),
                Entry::unavailable("New group"),
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
    use jstream_control::protocol::{
        Canvas, Crop, Finite32, ItemInfo, ItemTransform, PlacementForm, SceneId, SceneInfo,
        SceneRole, SourceInfo, SourceKind,
    };

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

    #[test]
    fn rows_list_the_top_of_the_stack_first() {
        let rows = rows(&collection(), &SceneId::new("main"));
        let ids: Vec<&str> = rows.iter().map(|r| r.item_id.as_str()).collect();
        assert_eq!(ids, ["top", "bottom"]);
        assert!(!rows[0].visible && rows[1].visible);
        assert_eq!(
            (rows[1].name.as_str(), rows[1].icon, rows[1].shared),
            ("Camera", "webcam", false)
        );
        assert_eq!(rows[0].name, "gone", "a missing source shows its ID");
        assert!(super::rows(&collection(), &SceneId::new("other")).is_empty());
    }
}
