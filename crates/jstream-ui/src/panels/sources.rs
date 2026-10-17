//! The sources list: the selected scene's items, top of the stack first, each named by
//! the source it places. An item is selected for the properties form, shown or hidden,
//! moved up or down the stack, or removed from the scene; its source is renamed, or
//! deleted everywhere through `delete.plan`. New items place a new source of a kind
//! `kind.list` reports, an existing source, or another scene.
//!
//! Docs: crates/jstream-ui/README.md#design

use crate::app::SessionContext;
use crate::panels::confirm::{Confirm, Pending};
use crate::panels::scenes::RenameField;
use crate::store::{Collection, Studio, apply, plan_delete, plan_summary};
use jstream_control::protocol::{
    DeleteTarget, Edit, ItemGeometry, ItemId, KindCategory, KindSummary, SceneId, Settings,
    SourceId, SourceKind,
};
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

/// What "Add" places, as the add form's select encodes it in its value.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Choice {
    /// A new source of this built-in kind.
    New(String),
    /// An existing source.
    Existing(SourceId),
    /// A new `scene` source showing this scene.
    Scene(SceneId),
}

impl Choice {
    fn encode(&self) -> String {
        match self {
            Choice::New(kind) => format!("new:{kind}"),
            Choice::Existing(id) => format!("source:{id}"),
            Choice::Scene(id) => format!("scene:{id}"),
        }
    }

    fn decode(value: &str) -> Option<Choice> {
        let (tag, rest) = value.split_once(':')?;
        match tag {
            "new" => Some(Choice::New(rest.into())),
            "source" => Some(Choice::Existing(rest.into())),
            "scene" => Some(Choice::Scene(rest.into())),
            _ => None,
        }
    }
}

/// The edits that place `choice` in `scene_id` as item `item_id`, covering the canvas:
/// a `source.create` first unless it places an existing source, then `item.add`.
fn place_edits(
    choice: Choice,
    scene_id: SceneId,
    item_id: ItemId,
    new_source_id: SourceId,
    name: String,
    collection: &Collection,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    let source_id = match choice {
        Choice::Existing(id) => id,
        Choice::New(kind) => {
            edits.push(source_create(
                new_source_id.clone(),
                name,
                SourceKind::Builtin { kind },
            ));
            new_source_id
        }
        Choice::Scene(shown) => {
            let name = if name.is_empty() {
                collection
                    .scene(&shown)
                    .map_or_else(|| shown.to_string(), |s| s.name.clone())
            } else {
                name
            };
            edits.push(source_create(
                new_source_id.clone(),
                name,
                SourceKind::Scene { scene_id: shown },
            ));
            new_source_id
        }
    };
    let canvas = collection.canvas;
    edits.push(Edit::ItemAdd {
        scene_id,
        item_id,
        source_id,
        transform: None,
        geometry: ItemGeometry::covering_canvas(canvas.width, canvas.height).ok(),
        opacity: None,
        blend: None,
    });
    edits
}

fn source_create(source_id: SourceId, name: String, kind: SourceKind) -> Edit {
    Edit::SourceCreate {
        source_id,
        name,
        kind,
        settings: Settings::new(),
        duplicated_from: None,
    }
}

/// The name a new source gets when the user gives none: the kind's display name.
fn default_name(choice: &Choice, kinds: &[KindSummary]) -> String {
    match choice {
        Choice::New(kind) => kinds
            .iter()
            .find(|k| &k.kind_id == kind)
            .map_or_else(|| kind.clone(), |k| k.name.clone()),
        _ => String::new(),
    }
}

/// The sources list.
#[component]
pub fn Sources() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let renaming = RwSignal::new(None::<SourceId>);
    let pending = RwSignal::new(None::<Pending>);
    let choice = RwSignal::new(String::new());
    let new_name = RwSignal::new(String::new());

    let scene = move || studio.selection.with(|s| s.scene.clone());
    let all_rows = move || {
        let Some(scene_id) = scene() else {
            return Vec::new();
        };
        studio
            .collection
            .with(|c| c.as_ref().map(|c| rows(c, &scene_id)).unwrap_or_default())
    };

    let add = move || {
        let (Some(s), Some(scene_id)) = (session.get(), scene()) else {
            return;
        };
        let Some(picked) = Choice::decode(&choice.get_untracked()) else {
            return;
        };
        let mut name = new_name.get_untracked().trim().to_owned();
        if name.is_empty() {
            name = studio
                .kinds
                .with_untracked(|k| default_name(&picked, k.as_deref().unwrap_or_default()));
        }
        let item_id = ItemId::new(studio.fresh_id(&s, "item"));
        let source_id = SourceId::new(studio.fresh_id(&s, "source"));
        let edits = studio.collection.with_untracked(|c| {
            c.as_ref()
                .map(|c| place_edits(picked, scene_id, item_id.clone(), source_id, name, c))
        });
        if let Some(edits) = edits {
            studio.apply(&s, edits);
            studio.select_item(Some(item_id));
            new_name.set(String::new());
        }
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
            move || studio.selection.with(|s| s.item.as_ref() == Some(&id))
        };
        let is_renaming = {
            let id = source_id.clone();
            move || renaming.with(|r| r.as_ref() == Some(&id))
        };
        let select = {
            let id = item_id.clone();
            move |_| studio.select_item(Some(id.clone()))
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
        let shown = name.clone();
        view! {
            <li class="row" class:selected=selected class:hidden=!visible>
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

    let options = move || {
        let current = scene();
        let (sources, scenes) = studio.collection.with(|c| {
            let Some(c) = c else {
                return (Vec::new(), Vec::new());
            };
            let sources: Vec<(String, String)> = c
                .sources
                .iter()
                .map(|s| {
                    (
                        Choice::Existing(s.source_id.clone()).encode(),
                        s.name.clone(),
                    )
                })
                .collect();
            let scenes: Vec<(String, String)> = c
                .scenes
                .iter()
                .filter(|s| Some(&s.scene_id) != current.as_ref())
                .map(|s| (Choice::Scene(s.scene_id.clone()).encode(), s.name.clone()))
                .collect();
            (sources, scenes)
        });
        let kinds: Vec<(String, String)> = studio.kinds.with(|k| {
            k.iter()
                .flatten()
                .filter(|k| k.category == KindCategory::Source)
                .map(|k| (Choice::New(k.kind_id.clone()).encode(), k.name.clone()))
                .collect()
        });
        let group = |label: &'static str, entries: Vec<(String, String)>| {
            (!entries.is_empty()).then(|| {
                view! {
                    <optgroup label=label>
                        {entries
                            .into_iter()
                            .map(|(value, text)| view! { <option value=value>{text}</option> })
                            .collect_view()}
                    </optgroup>
                }
            })
        };
        view! {
            <option value="">"Add…"</option>
            {group("New source", kinds)}
            {group("Existing source", sources)}
            {group("Scene", scenes)}
        }
    };

    view! {
        <section class="panel sources-panel">
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
                <form class="add" on:submit=move |event| {
                    event.prevent_default();
                    add();
                }>
                    <select
                        aria-label="What to add"
                        prop:value=move || choice.get()
                        on:change=move |event| choice.set(event_target_value(&event))
                    >
                        {options}
                    </select>
                    <input type="text" placeholder="Name (optional)" bind:value=new_name />
                    <button type="submit" disabled=move || choice.with(String::is_empty)>"Add"</button>
                </form>
            </Show>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::{Choice, default_name, place_edits, rows};
    use crate::store::Collection;
    use jstream_control::protocol::{
        Canvas, Crop, Edit, Finite32, ItemInfo, ItemTransform, KindCategory, KindSummary,
        PlacementForm, SceneId, SceneInfo, SceneRole, SourceId, SourceInfo, SourceKind,
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

    #[test]
    fn choices_round_trip_through_the_select() {
        for choice in [
            Choice::New("color".into()),
            Choice::Existing("a:b".into()),
            Choice::Scene("s".into()),
        ] {
            assert_eq!(Choice::decode(&choice.encode()), Some(choice));
        }
        assert_eq!(Choice::decode(""), None);
        assert_eq!(Choice::decode("bogus:x"), None);
    }

    #[test]
    fn placing_creates_the_source_and_covers_the_canvas() {
        let c = collection();
        let edits = place_edits(
            Choice::New("color".into()),
            "main".into(),
            "i".into(),
            "s".into(),
            "Backdrop".into(),
            &c,
        );
        let [
            Edit::SourceCreate {
                source_id, kind, ..
            },
            Edit::ItemAdd { geometry, .. },
        ] = edits.as_slice()
        else {
            panic!("expected source.create then item.add: {edits:?}");
        };
        assert_eq!(source_id, &SourceId::new("s"));
        assert_eq!(
            kind,
            &SourceKind::Builtin {
                kind: "color".into()
            }
        );
        let geometry = geometry.unwrap_or_else(|| panic!("no geometry"));
        assert_eq!(
            (geometry.size.width.get(), geometry.size.height.get()),
            (1280.0, 720.0)
        );

        let existing = place_edits(
            Choice::Existing("cam".into()),
            "main".into(),
            "i".into(),
            "unused".into(),
            String::new(),
            &c,
        );
        assert!(
            matches!(existing.as_slice(), [Edit::ItemAdd { source_id, .. }] if source_id.as_str() == "cam")
        );

        let nested = place_edits(
            Choice::Scene("main".into()),
            "other".into(),
            "i".into(),
            "s".into(),
            String::new(),
            &c,
        );
        assert!(
            matches!(&nested[0], Edit::SourceCreate { name, kind: SourceKind::Scene { .. }, .. } if name == "Main"),
            "a nested scene is named after its scene"
        );
    }

    #[test]
    fn a_new_source_is_named_after_its_kind() {
        let kinds = [KindSummary {
            kind_id: "color".into(),
            category: KindCategory::Source,
            name: "Colour".into(),
            version: 1,
        }];
        assert_eq!(default_name(&Choice::New("color".into()), &kinds), "Colour");
        assert_eq!(default_name(&Choice::New("text".into()), &kinds), "text");
        assert_eq!(default_name(&Choice::Existing("x".into()), &kinds), "");
    }
}
