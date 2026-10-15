//! The scene list: the collection's scenes in display order, with the one on program
//! marked. Selecting a scene shows it in the sources list and the canvas; "Program"
//! puts it on program (`scene.switch`). Scenes are added, renamed, reordered and
//! deleted here, deletes through `delete.plan` so the user sees what else goes.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::app::SessionContext;
use crate::panels::confirm::{Confirm, Pending};
use crate::store::{Studio, apply, plan_delete, plan_summary};
use jstream_control::protocol::{Action, DeleteTarget, Edit, SceneId};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// One row of the list, as the view needs it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Row {
    scene_id: SceneId,
    name: String,
    /// The index, among all scenes (which `scene.reorder` counts in), of the listed
    /// scene above and below it, if there is one.
    up: Option<u32>,
    down: Option<u32>,
}

/// The rows: listed scenes only (groups are hidden), each with where a move up or down
/// takes it among all scenes.
fn rows(studio: Studio) -> Vec<Row> {
    studio.collection.with(|c| {
        let Some(c) = c else { return Vec::new() };
        let listed: Vec<(u32, &jstream_control::protocol::SceneInfo)> = c
            .scenes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.role == jstream_control::protocol::SceneRole::Scene)
            .map(|(i, s)| (u32::try_from(i).unwrap_or(u32::MAX), s))
            .collect();
        listed
            .iter()
            .enumerate()
            .map(|(at, (_, scene))| Row {
                scene_id: scene.scene_id.clone(),
                name: scene.name.clone(),
                up: at.checked_sub(1).map(|a| listed[a].0),
                down: listed.get(at + 1).map(|(i, _)| *i),
            })
            .collect()
    })
}

/// The scene list.
#[component]
pub fn Scenes() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let new_name = RwSignal::new(String::new());
    let renaming = RwSignal::new(None::<SceneId>);
    let pending = RwSignal::new(None::<Pending>);

    let add = move || {
        let Some(s) = session.get() else { return };
        let name = new_name.get_untracked().trim().to_owned();
        let count = studio
            .collection
            .with_untracked(|c| c.as_ref().map_or(0, |c| c.listed_scenes().count()));
        let name = if name.is_empty() {
            format!("Scene {}", count + 1)
        } else {
            name
        };
        let scene_id = SceneId::new(studio.fresh_id(&s, "scene"));
        studio.apply(
            &s,
            vec![Edit::SceneCreate {
                scene_id: scene_id.clone(),
                name,
            }],
        );
        studio.select_scene(scene_id);
        new_name.set(String::new());
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

    let row = move |row: Row| {
        let Row {
            scene_id,
            name,
            up,
            down,
        } = row;
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
                        },
                    );
                }
            }
        };
        let reorder = {
            let id = scene_id.clone();
            move |to: u32| {
                if let Some(s) = session.get() {
                    studio.apply(
                        &s,
                        vec![Edit::SceneReorder {
                            scene_id: id.clone(),
                            index: to,
                        }],
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
                if to.is_empty() || to == old {
                    return;
                }
                if let Some(s) = session.get() {
                    studio.apply(
                        &s,
                        vec![Edit::SceneRename {
                            scene_id: id.clone(),
                            name: to,
                        }],
                    );
                }
            }
        };
        let start_rename = {
            let id = scene_id.clone();
            move |_| renaming.set(Some(id.clone()))
        };
        let delete = {
            let id = scene_id.clone();
            let name = name.clone();
            move |_| ask_delete(id.clone(), name.clone())
        };
        let shown_name = name.clone();
        let reorder_up = reorder.clone();
        view! {
            <li class="row" class:selected=selected aria-current=move || on_program.get().then_some("true")>
                <Show
                    when=is_renaming
                    fallback=move || {
                        view! {
                            <button class="row-main" on:click=select.clone() on:dblclick=start_rename.clone()>
                                {shown_name.clone()}
                            </button>
                        }
                    }
                >
                    <RenameField value=name.clone() done=rename.clone() />
                </Show>
                <Show when=move || on_program.get()>
                    <span class="badge live">"Program"</span>
                </Show>
                <span class="row-tools">
                    <Show when=move || !on_program.get()>
                        <button title="Put this scene on program" on:click=to_program.clone()>
                            "Program"
                        </button>
                    </Show>
                    <button
                        title="Move up"
                        disabled=up.is_none()
                        on:click={
                            let reorder = reorder_up.clone();
                            move |_| if let Some(to) = up { reorder(to) }
                        }
                    >"▲"</button>
                    <button
                        title="Move down"
                        disabled=down.is_none()
                        on:click={
                            let reorder = reorder.clone();
                            move |_| if let Some(to) = down { reorder(to) }
                        }
                    >"▼"</button>
                    <button title="Delete scene" on:click=delete.clone()>"✕"</button>
                </span>
            </li>
        }
    };

    view! {
        <section class="panel scenes-panel">
            <h2>"Scenes"</h2>
            <Confirm pending=pending confirm=confirm_delete />
            <ul class="rows">
                <For each=move || rows(studio) key=|row| row.clone() children=row />
            </ul>
            <Show when=move || studio.collection.with(|c| c.as_ref().is_some_and(|c| c.listed_scenes().next().is_none()))>
                <p class="placeholder">"No scenes yet."</p>
            </Show>
            <form class="add" on:submit=move |event| {
                event.prevent_default();
                add();
            }>
                <input type="text" placeholder="New scene name" bind:value=new_name />
                <button type="submit">"Add"</button>
            </form>
        </section>
    }
}

/// A text field that edits a name in place: Enter or leaving the field commits, Escape
/// cancels (commits the old name, which changes nothing).
#[component]
pub fn RenameField(value: String, done: impl Fn(String) + Clone + 'static) -> impl IntoView {
    let text = RwSignal::new(value.clone());
    // Enter commits and then the field's removal blurs it: commit once.
    let finished = StoredValue::new(false);
    let finish = move |name: String| {
        if !finished.get_value() {
            finished.set_value(true);
            done(name);
        }
    };
    let commit = {
        let finish = finish.clone();
        move || finish(text.get_untracked())
    };
    let on_blur = commit.clone();
    view! {
        <input
            class="rename"
            type="text"
            autofocus
            bind:value=text
            on:blur=move |_| on_blur()
            on:keydown=move |event| match event.key().as_str() {
                "Enter" => commit(),
                "Escape" => finish(value.clone()),
                _ => {}
            }
        />
    }
}
