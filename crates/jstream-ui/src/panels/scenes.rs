//! The Scenes panel (spec-panels.md 1): the collection's scenes in display order, under
//! a search box and a favourites filter, each row with its icon, a "program" badge on
//! the scene on program and a favourite star. Selecting a scene shows it in Scene
//! objects and the canvas; the hover button puts it on program (`scene.switch`). Rows
//! are reordered by dragging them (`scene.reorder`), renamed in place (F2 or a double
//! click) and deleted (Del or the menu) through `delete.plan`, so the user sees what
//! else goes. A right-click on a scene opens its menu, and one on the panel's empty
//! space opens the panel's (spec-workspace.md 5.2.3 and 5.2.7).
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
use crate::panels::tree::{Filter, Reorder, SearchBar, Side, Star, dropped_at};
use crate::store::{Collection, Studio, apply, plan_delete, plan_summary, reset_edits};
use jstream_control::protocol::{Action, DeleteTarget, Edit, SceneId, SceneInfo};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::HashSet;

/// One row of the list, as the view needs it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Row {
    scene_id: SceneId,
    name: String,
    /// The scene's icon, the keyframe diamond for a keyframe scene, or the scene icon.
    icon: &'static str,
    favourite: bool,
    /// For a keyframe scene, its parent and the parent's name.
    keyframe_of: Option<(SceneId, String)>,
    /// Whether the scene has keyframe scenes, so its row has a caret.
    keyframes: bool,
}

impl Row {
    fn of(scene: &SceneInfo, keyframe_of: Option<&SceneInfo>, keyframes: bool) -> Row {
        Row {
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
            keyframe_of: keyframe_of.map(|p| (p.scene_id.clone(), p.name.clone())),
            keyframes,
        }
    }
}

/// The rows: listed scenes only (groups are hidden), in display order, each followed
/// by its keyframe scenes in theirs unless it is folded. A keyframe scene whose parent
/// isn't listed shows on its own.
fn rows(collection: &Collection, folded: &HashSet<SceneId>) -> Vec<Row> {
    let listed: Vec<&SceneInfo> = collection.listed_scenes().collect();
    let is_listed = |id: &SceneId| listed.iter().any(|s| &s.scene_id == id);
    let mut rows = Vec::new();
    for scene in &listed {
        if scene.variant_of.as_ref().is_some_and(is_listed) {
            continue;
        }
        let keyframes: Vec<&SceneInfo> = listed
            .iter()
            .filter(|k| k.variant_of.as_ref() == Some(&scene.scene_id))
            .copied()
            .collect();
        rows.push(Row::of(
            scene,
            collection.parent_of(&scene.scene_id),
            !keyframes.is_empty(),
        ));
        if !folded.contains(&scene.scene_id) {
            rows.extend(
                keyframes
                    .into_iter()
                    .map(|k| Row::of(k, Some(scene), false)),
            );
        }
    }
    rows
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
    let renaming = RwSignal::new(None::<SceneId>);
    let pending = RwSignal::new(None::<Pending>);
    let filter = Filter::new();
    let reorder = Reorder::new();
    // The scenes whose keyframes are folded away.
    let folded = RwSignal::new(HashSet::<SceneId>::new());

    let menus = use_context::<Menus>();
    let all_rows = move || {
        folded.with(|folded| {
            studio
                .collection
                .with(|c| c.as_ref().map(|c| rows(c, folded)).unwrap_or_default())
        })
    };
    let shown_rows = move || {
        all_rows()
            .into_iter()
            .filter(|row| filter.shows(&row.name, row.favourite))
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
            let c = c.as_ref()?;
            // Keyframes stay under their scene.
            if c.is_keyframe(&SceneId::new(target.as_str())) {
                return None;
            }
            let order: Vec<&str> = c.scenes.iter().map(|s| s.scene_id.as_str()).collect();
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
            keyframe_of,
            keyframes,
        } = row;
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
            let id = scene_id.clone();
            move || renaming.with(|r| r.as_ref() == Some(&id))
        };
        let select = {
            let id = scene_id.clone();
            move |_| studio.select_scene(id.clone())
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
        let start_rename = {
            let id = scene_id.clone();
            move |_| renaming.set(Some(id.clone()))
        };
        // F2 renames and Del deletes, the keys the menu's hints name.
        let on_key = {
            let id = scene_id.clone();
            let name = name.clone();
            let parent = keyframe_of.clone();
            move |event: leptos::ev::KeyboardEvent| match event.key().as_str() {
                "F2" => {
                    event.prevent_default();
                    renaming.set(Some(id.clone()));
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
        // prototype shows them disabled, and so does Show in preview. A keyframe's menu
        // is its own.
        let on_row_context = {
            let id = scene_id.clone();
            let name = name.clone();
            let parent = keyframe_of.clone();
            move |event: leptos::ev::MouseEvent| {
                let Some(menus) = menus else { return };
                let rename_id = id.clone();
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
                            Entry::item("Rename", move || renaming.set(Some(rename_id.clone())))
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
                let keyframe_id = id.clone();
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
                        Entry::item("New keyframe", move || new_keyframe(keyframe_id.clone())),
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
                if !is_keyframe && !filter.active() && renaming.with_untracked(Option::is_none) {
                    reorder.down(&key, &event);
                }
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
        let shown_name = name.clone();
        let star_name = name.clone();
        view! {
            <li
                class="row tree-row"
                class:keyframe=is_keyframe
                title=title
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
                <span class="tree-caret">{caret}</span>
                <Show
                    when=is_renaming
                    fallback=move || {
                        let name = shown_name.clone();
                        view! {
                            <button class="row-main" on:click=select.clone() on:dblclick=start_rename.clone()>
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

#[cfg(test)]
mod tests {
    use super::{keyframe_name, rows};
    use crate::store::Collection;
    use jstream_control::protocol::{SceneId, SceneInfo, SceneRole};
    use std::collections::HashSet;

    fn scene(id: &str, variant_of: Option<&str>) -> SceneInfo {
        SceneInfo {
            overrides: Vec::new(),
            filters: Default::default(),
            variant_of: variant_of.map(SceneId::new),
            scene_id: id.into(),
            name: id.to_uppercase(),
            role: SceneRole::Scene,
            folder: None,
            meta: Default::default(),
            items: Vec::new(),
            transition_in: None,
            transition_out: None,
        }
    }

    #[test]
    fn keyframes_nest_under_their_scene_and_fold_away() {
        let c = Collection {
            scenes: vec![
                scene("k2", Some("b")),
                scene("a", None),
                scene("b", None),
                scene("k1", Some("b")),
            ],
            ..Collection::default()
        };
        let ids = |folded: &HashSet<SceneId>| {
            rows(&c, folded)
                .into_iter()
                .map(|r| r.scene_id.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&HashSet::new()), ["a", "b", "k2", "k1"]);
        let all = rows(&c, &HashSet::new());
        assert!(all[1].keyframes && !all[0].keyframes);
        assert_eq!(all[2].icon, "keyframe");
        assert_eq!(
            all[2].keyframe_of,
            Some((SceneId::new("b"), "B".to_owned()))
        );
        assert_eq!(ids(&HashSet::from([SceneId::new("b")])), ["a", "b"]);
        assert_eq!(keyframe_name(2), "Keyframe 3");
    }
}
