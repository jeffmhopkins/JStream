//! The Scenes panel (spec-panels.md 1): the collection's scenes in display order, under
//! a search box and a favourites filter, each row with its icon, a "program" badge on
//! the scene on program and a favourite star. Selecting a scene shows it in Scene
//! objects and the canvas; the hover button puts it on program (`scene.switch`). Rows
//! are reordered by dragging them (`scene.reorder`), renamed in place (F2 or a double
//! click) and deleted (Del or the menu) through `delete.plan`, so the user sees what
//! else goes. A right-click on a scene opens its menu, and one on the panel's empty
//! space opens the panel's (spec-workspace.md 5.2.3 and 5.2.7).
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#scenes

use crate::app::SessionContext;
use crate::icons::{Icon, named};
use crate::menu::{Entry, Menus};
use crate::panels::confirm::{Confirm, Pending};
use crate::panels::tree::{Filter, Reorder, SearchBar, Side, Star, dropped_at};
use crate::store::{Collection, Studio, apply, plan_delete, plan_summary};
use jstream_control::protocol::{Action, DeleteTarget, Edit, SceneId};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// One row of the list, as the view needs it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Row {
    scene_id: SceneId,
    name: String,
    /// The scene's icon, or the scene icon.
    icon: &'static str,
    favourite: bool,
}

/// The rows: listed scenes only (groups are hidden), in display order.
fn rows(collection: &Collection) -> Vec<Row> {
    collection
        .listed_scenes()
        .map(|scene| Row {
            scene_id: scene.scene_id.clone(),
            name: scene.name.clone(),
            icon: scene
                .meta
                .icon
                .as_deref()
                .map_or("layers", |icon| named(icon, "layers")),
            favourite: scene.meta.favourite,
        })
        .collect()
}

/// The scene list.
#[component]
pub fn Scenes() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let renaming = RwSignal::new(None::<SceneId>);
    let pending = RwSignal::new(None::<Pending>);
    let filter = Filter::new();
    let reorder = Reorder::new();

    let menus = use_context::<Menus>();
    let all_rows = move || {
        studio
            .collection
            .with(|c| c.as_ref().map(rows).unwrap_or_default())
    };
    let shown_rows = move || {
        all_rows()
            .into_iter()
            .filter(|row| filter.shows(&row.name, row.favourite, &[]))
            .collect::<Vec<_>>()
    };
    // Adds a scene named "Scene N", selects it and returns its ID.
    let add = move || -> Option<SceneId> {
        let s = session.get()?;
        let count = studio
            .collection
            .with_untracked(|c| c.as_ref().map_or(0, |c| c.listed_scenes().count()));
        let scene_id = SceneId::new(studio.fresh_id(&s, "scene"));
        studio.apply(
            &s,
            vec![Edit::SceneCreate {
                scene_id: scene_id.clone(),
                name: format!("Scene {}", count + 1),
                role: None,
            }],
        );
        studio.select_scene(scene_id.clone());
        Some(scene_id)
    };
    // The menus' New scene: a new scene, with its name open for editing.
    let new_scene = move || {
        if let Some(scene_id) = add() {
            renaming.set(Some(scene_id));
        }
    };
    let send = move |edit: Edit| {
        if let Some(s) = session.get() {
            studio.apply(&s, vec![edit]);
        }
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
    let confirm_delete = move |plan: Pending| {
        let Some(s) = session.get() else { return };
        spawn_local(async move {
            if let Err(error) = apply(&s, plan.edits, Some(plan.rev)).await {
                studio.report(&error);
            }
        });
    };
    // A dragged scene goes before or after the one it is dropped on, counted among all
    // scenes, groups included, as `scene.reorder` counts.
    let drop = move |moved: String, target: String, side: Side| {
        let index = studio.collection.with_untracked(|c| {
            let order: Vec<&str> = c
                .as_ref()?
                .scenes
                .iter()
                .map(|s| s.scene_id.as_str())
                .collect();
            dropped_at(&order, &moved.as_str(), &target.as_str(), side)
        });
        if let Some(index) = index {
            send(Edit::SceneReorder {
                scene_id: SceneId::new(moved),
                index,
            });
        }
    };

    let row = move |row: Row| {
        let Row {
            scene_id,
            name,
            icon,
            favourite,
        } = row;
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
            let id = scene_id.clone();
            move || renaming.with(|r| r.as_ref() == Some(&id))
        };
        let select = {
            let id = scene_id.clone();
            move |_| studio.select_scene(id.clone())
        };
        let to_program = {
            let id = scene_id.clone();
            move |_| {
                if let Some(s) = session.get() {
                    studio.act(
                        &s,
                        Action::SceneSwitch {
                            scene_id: id.clone(),
                            preset_id: None,
                        },
                    );
                }
            }
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
        let start_rename = {
            let id = scene_id.clone();
            move |_| renaming.set(Some(id.clone()))
        };
        // F2 renames and Del deletes, the keys the menu's hints name.
        let on_key = {
            let id = scene_id.clone();
            let name = name.clone();
            move |event: leptos::ev::KeyboardEvent| match event.key().as_str() {
                "F2" => {
                    event.prevent_default();
                    renaming.set(Some(id.clone()));
                }
                "Delete" => {
                    event.prevent_default();
                    ask_delete(id.clone(), name.clone());
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
        // prototype shows them disabled, and so does Show in preview.
        let on_row_context = {
            let id = scene_id.clone();
            let name = name.clone();
            move |event: leptos::ev::MouseEvent| {
                let Some(menus) = menus else { return };
                let rename_id = id.clone();
                let preview_id = id.clone();
                let (delete_id, delete_name) = (id.clone(), name.clone());
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
                        Entry::item("Rename", move || renaming.set(Some(rename_id.clone())))
                            .hint("F2"),
                        Entry::Sep,
                        Entry::submenu("Transition in", Vec::new()),
                        Entry::submenu("Transition out", Vec::new()),
                        Entry::Sep,
                        Entry::unavailable("Properties…").hint("Double-click"),
                        Entry::unavailable("Edit filters…"),
                        Entry::submenu("Pop out", Vec::new()),
                        Entry::Sep,
                        Entry::item("New scene", new_scene),
                        Entry::unavailable("New keyframe"),
                        Entry::unavailable("New folder"),
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
        let shown_name = name.clone();
        let star_name = name.clone();
        view! {
            <li
                class="row tree-row"
                class:selected=selected
                class:drop-before=drop_side(Side::Before)
                class:drop-after=drop_side(Side::After)
                class:dragging=dragging
                data-tree-id=key
                aria-current=move || on_program.get().then_some("true")
                on:contextmenu=on_row_context
                on:keydown=on_key
                on:pointerdown=press
            >
                <span class="tree-caret"></span>
                <Show
                    when=is_renaming
                    fallback=move || {
                        let name = shown_name.clone();
                        view! {
                            <button class="row-main" on:click=select.clone() on:dblclick=start_rename.clone()>
                                <Icon name=icon size=15 />
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
                <Show when=move || !on_program.get()>
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
    };

    // The panel's own menu, on its empty space (5.2.3).
    let on_context = move |event: leptos::ev::MouseEvent| {
        let Some(menus) = menus else { return };
        menus.open(
            &event,
            vec![
                Entry::item("New scene", new_scene),
                Entry::unavailable("New folder"),
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
                let none = all_rows().is_empty();
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
