//! The sources list: the selected scene's items, top of the stack first, each named by
//! the source it places. An item is selected for the properties form, shown or hidden,
//! moved up or down the stack, or removed from the scene; its source is renamed, or
//! deleted everywhere through `delete.plan`. A right-click on a row opens the selected
//! items' menu (`item_menu.rs`), and one on the panel's empty space opens the panel's
//! menu, whose Add source opens the Add source dialog (`add_source.rs`).
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::app::SessionContext;
use crate::item_menu;
use crate::menu::{Entry, Menus};
use crate::panels::add_source::AddSourceDialog;
use crate::panels::confirm::{Confirm, Pending};
use crate::panels::scenes::RenameField;
use crate::store::{Collection, Studio, apply, plan_delete, plan_summary};
use jstream_control::protocol::{DeleteTarget, Edit, ItemId, SceneId, SourceId};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// One row of the list, as the view needs it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Row {
    item_id: ItemId,
    source_id: SourceId,
    name: String,
    kind: String,
    visible: bool,
    /// Its stacking index (0 is the bottom), and the scene's item count.
    index: u32,
    count: u32,
}

/// The selected scene's items, top of the stack first.
fn rows(collection: &Collection, scene_id: &SceneId) -> Vec<Row> {
    let Some(scene) = collection.scene(scene_id) else {
        return Vec::new();
    };
    let count = u32::try_from(scene.items.len()).unwrap_or(u32::MAX);
    scene
        .items
        .iter()
        .enumerate()
        .rev()
        .map(|(index, item)| Row {
            item_id: item.item_id.clone(),
            source_id: item.source_id.clone(),
            name: collection.source_name(&item.source_id),
            kind: collection
                .source(&item.source_id)
                .map_or_else(String::new, |s| collection.kind_label(&s.kind)),
            visible: item.visible,
            index: u32::try_from(index).unwrap_or(u32::MAX),
            count,
        })
        .collect()
}

/// The sources list.
#[component]
pub fn Sources() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let renaming = RwSignal::new(None::<SourceId>);
    let pending = RwSignal::new(None::<Pending>);
    let menus = use_context::<Menus>();
    let adder = use_context::<AddSourceDialog>();
    let item_cx = item_menu::Context::from_page();

    let scene = move || studio.selection.with(|s| s.scene.clone());
    let all_rows = move || {
        let Some(scene_id) = scene() else {
            return Vec::new();
        };
        studio
            .collection
            .with(|c| c.as_ref().map(|c| rows(c, &scene_id)).unwrap_or_default())
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

    let row = move |row: Row| {
        let Row {
            item_id,
            source_id,
            name,
            kind,
            visible,
            index,
            count,
        } = row;
        let selected = {
            let id = item_id.clone();
            move || studio.selection.with(|s| s.contains(&id))
        };
        let is_renaming = {
            let id = source_id.clone();
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
        let start_rename = {
            let id = source_id.clone();
            move |_| renaming.set(Some(id.clone()))
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
        let item = item_id.clone();
        let toggle = move |_: leptos::ev::MouseEvent| {
            send(Edit::ItemSetVisible {
                item_id: item.clone(),
                scene_id: None,
                visible: !visible,
            });
        };
        let item = item_id.clone();
        let raise = move |_: leptos::ev::MouseEvent| {
            send(Edit::ItemReorder {
                item_id: item.clone(),
                index: index + 1,
            });
        };
        let item = item_id.clone();
        let lower = move |_: leptos::ev::MouseEvent| {
            send(Edit::ItemReorder {
                item_id: item.clone(),
                index: index.saturating_sub(1),
            });
        };
        let item = item_id.clone();
        let remove = move |_: leptos::ev::MouseEvent| {
            send(Edit::ItemRemove {
                item_id: item.clone(),
            })
        };
        let delete = {
            let id = source_id.clone();
            let name = name.clone();
            move |_| ask_delete(id.clone(), name.clone())
        };
        // A right-click on the row opens the selected items' menu, after selecting the
        // item alone unless it is already selected.
        let on_row_context = {
            let id = item_id.clone();
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
                let entries = item_menu::entries(cx, &scene_id, &items, item_menu::Scope::Objects);
                if !entries.is_empty() {
                    menus.open(&event, entries);
                }
            }
        };
        let shown = name.clone();
        view! {
            <li class="row" class:selected=selected class:hidden=!visible on:contextmenu=on_row_context>
                <button
                    class="eye"
                    title=if visible { "Hide" } else { "Show" }
                    aria-pressed=if visible { "true" } else { "false" }
                    on:click=toggle
                >{if visible { "●" } else { "○" }}</button>
                <Show
                    when=is_renaming
                    fallback=move || {
                        view! {
                            <button class="row-main" on:click=select.clone() on:dblclick=start_rename.clone()>
                                {shown.clone()}
                                <span class="muted">{kind.clone()}</span>
                            </button>
                        }
                    }
                >
                    <RenameField value=name.clone() done=rename.clone() />
                </Show>
                <span class="row-tools">
                    <button title="Move up" disabled={index + 1 >= count} on:click=raise>"▲"</button>
                    <button title="Move down" disabled={index == 0} on:click=lower>"▼"</button>
                    <button title="Remove from this scene" on:click=remove>"−"</button>
                    <button title="Delete the source everywhere" on:click=delete>"✕"</button>
                </span>
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
        <section class="panel sources-panel" on:contextmenu=on_context>
            <h2>"Sources"</h2>
            <Confirm pending=pending confirm=confirm_delete />
            <Show
                when=move || scene().is_some()
                fallback=|| view! { <p class="placeholder">"Select a scene."</p> }
            >
                <ul class="rows">
                    <For each=all_rows key=|row| row.clone() children=row />
                </ul>
                <Show when=move || all_rows().is_empty()>
                    <p class="placeholder">"This scene is empty."</p>
                </Show>
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
        assert_eq!(
            (rows[0].index, rows[0].count, rows[0].visible),
            (1, 2, false)
        );
        assert_eq!(
            (rows[1].name.as_str(), rows[1].kind.as_str()),
            ("Camera", "webcam")
        );
        assert_eq!(rows[0].name, "gone", "a missing source shows its ID");
        assert!(super::rows(&collection(), &SceneId::new("other")).is_empty());
    }
}
