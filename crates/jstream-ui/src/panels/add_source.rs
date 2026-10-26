//! The Add source dialog: the source types the engine lists, grouped as the prototype
//! groups them (Capture, Audio, Media and web, Generated, Scenes), each with its icon and
//! description. A type is created new under a name, or an existing source of that type
//! is placed again as a reference; the Scene type nests another scene. A new visual
//! source is placed centred at the prototype's native size for its type, and an
//! existing source placed nowhere at its own size when the engine reports one, each
//! scaled down to fit the canvas, then selected with the Inspector shown.
//!
//! Docs: docs/UI-FEATURES.md#dialogs

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::dock::PanelId;
use crate::icons::Icon;
use crate::panels::source_properties::PropertiesDialog;
use crate::store::{Collection, Sizes, Studio, apply};
use crate::workspace::DockState;
use jstream_control::protocol::{
    Canvas, Edit, ItemGeometry, ItemId, KindCategory, KindSummary, SceneId, Settings, Size,
    SourceId, SourceKind, Vec2,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// A group of the type list, in the order the list shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    /// Displays, windows and video devices.
    Capture,
    /// Audio-only sources.
    Audio,
    /// Files and web pages.
    MediaAndWeb,
    /// Text, colour and audio visualisers.
    Generated,
    /// Kinds the prototype doesn't name: plugin kinds, and built-ins it lacks.
    Other,
    /// Another scene, nested.
    Scenes,
}

impl Category {
    /// The group's head.
    fn title(self) -> &'static str {
        match self {
            Category::Capture => "CAPTURE",
            Category::Audio => "AUDIO",
            Category::MediaAndWeb => "MEDIA AND WEB",
            Category::Generated => "GENERATED",
            Category::Other => "OTHER",
            Category::Scenes => "SCENES",
        }
    }
}

/// One source type the dialog offers.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceType {
    /// The built-in kind it creates, or `None` for the Scene type.
    pub kind: Option<String>,
    /// Its name in the list.
    pub name: String,
    /// What it is, under its name in the right pane.
    pub description: String,
    /// Its icon.
    pub icon: &'static str,
    /// The group it is listed under.
    pub category: Category,
    /// Its native size on the canvas, or `None` for an audio-only type.
    pub native: Option<(f32, f32)>,
    /// Whether a new source of it can be created: the engine lists its kind.
    pub creatable: bool,
    /// Its place within its group: the prototype's order, then the engine's name.
    rank: usize,
}

impl SourceType {
    /// The key the list and the radio buttons use for it.
    fn key(&self) -> String {
        self.kind.clone().unwrap_or_else(|| SCENE_KEY.into())
    }
}

/// The Scene type's key: no built-in kind has this ID, which isn't a valid one.
const SCENE_KEY: &str = "#scene";

/// A known kind's name, description, icon, group and native size.
type Known = (
    &'static str,
    &'static str,
    &'static str,
    Category,
    Option<(f32, f32)>,
);

/// The prototype's entry for a built-in kind (`SOURCE_TYPES`, spec-workspace.md 6.5).
/// The kinds it doesn't know keep the engine's name.
fn known(kind: &str) -> Option<Known> {
    use Category::{Audio, Capture, Generated, MediaAndWeb};
    Some(match kind {
        "display" => (
            "Display capture",
            "A whole monitor, through Windows Graphics Capture.",
            "monitor",
            Capture,
            Some((1920.0, 1080.0)),
        ),
        "window" => (
            "Window capture",
            "One application window.",
            "window",
            Capture,
            Some((1280.0, 720.0)),
        ),
        "webcam" => (
            "Video capture device",
            "A webcam or capture card.",
            "webcam",
            Capture,
            Some((1280.0, 720.0)),
        ),
        "audio_input" => (
            "Audio input",
            "A microphone or line input.",
            "mic",
            Audio,
            None,
        ),
        "desktop_audio" => (
            "Desktop audio",
            "Everything a playback device plays.",
            "speaker",
            Audio,
            None,
        ),
        "app_audio" => (
            "Application audio",
            "Sound from one application.",
            "music",
            Audio,
            None,
        ),
        "media" => (
            "Media file",
            "A video or audio file, with looping.",
            "film",
            MediaAndWeb,
            Some((1280.0, 720.0)),
        ),
        "image" => (
            "Image",
            "PNG, JPG, WebP or animated GIF.",
            "image",
            MediaAndWeb,
            Some((400.0, 400.0)),
        ),
        "browser" => (
            "Browser",
            "A web page or widget.",
            "globe",
            MediaAndWeb,
            Some((800.0, 600.0)),
        ),
        "text" => (
            "Text",
            "Styled text.",
            "text",
            Generated,
            Some((800.0, 90.0)),
        ),
        "color" => (
            "Color",
            "A solid fill. Handy as a mask or matte.",
            "palette",
            Generated,
            Some((1920.0, 1080.0)),
        ),
        "waveform" => (
            "Audio waveform",
            "Draws an audio source as a moving wave.",
            "wave",
            Generated,
            Some((900.0, 200.0)),
        ),
        "spectrum" => (
            "Audio spectrum",
            "Draws an audio source as frequency bars.",
            "bars",
            Generated,
            Some((900.0, 240.0)),
        ),
        _ => return None,
    })
}

/// The prototype's order of the kinds it knows.
const ORDER: [&str; 13] = [
    "display",
    "window",
    "webcam",
    "audio_input",
    "desktop_audio",
    "app_audio",
    "media",
    "image",
    "browser",
    "text",
    "color",
    "waveform",
    "spectrum",
];

/// The prototype's name and icon for a built-in kind, if it knows the kind.
pub fn type_label(kind: &str) -> Option<(&'static str, &'static str)> {
    known(kind).map(|(name, _, icon, ..)| (name, icon))
}

/// Whether a source of this built-in kind draws a picture: every kind but the
/// audio-only ones. A kind the prototype doesn't know is taken to have one.
pub fn has_picture(kind: &str) -> bool {
    known(kind).is_none_or(|(.., native)| native.is_some())
}

/// The types to offer: every source kind `kind.list` reported, the kinds of existing
/// sources it didn't (which can be placed again but not created), then Scene, grouped
/// and in the prototype's order. A kind the prototype lacks goes under Other, by name,
/// placed like a 1280 × 720 video.
pub fn catalogue(kinds: &[KindSummary], placed: &[&str]) -> Vec<SourceType> {
    let listed = kinds
        .iter()
        .filter(|k| k.category == KindCategory::Source)
        .map(|k| (k.kind_id.as_str(), k.name.as_str(), true));
    let mut unlisted: Vec<&str> = placed
        .iter()
        .copied()
        .filter(|p| !kinds.iter().any(|k| k.kind_id == *p))
        .collect();
    unlisted.sort_unstable();
    unlisted.dedup();
    let mut types: Vec<SourceType> = listed
        .chain(unlisted.into_iter().map(|id| (id, id, false)))
        .map(|(kind_id, engine_name, creatable)| match known(kind_id) {
            Some((name, description, icon, category, native)) => SourceType {
                kind: Some(kind_id.to_owned()),
                name: name.into(),
                description: description.into(),
                icon,
                category,
                native,
                creatable,
                rank: ORDER.iter().position(|o| *o == kind_id).unwrap_or(0),
            },
            None => SourceType {
                kind: Some(kind_id.to_owned()),
                name: engine_name.to_owned(),
                description: String::new(),
                icon: match kind_id {
                    "chat_message" | "youtube_chat" => "chat",
                    _ => "tag",
                },
                category: Category::Other,
                native: Some((1280.0, 720.0)),
                creatable,
                rank: ORDER.len(),
            },
        })
        .collect();
    types.push(SourceType {
        kind: None,
        name: "Scene".into(),
        description: "Nest another scene inside this one.".into(),
        icon: "layers",
        category: Category::Scenes,
        native: None,
        creatable: false,
        rank: 0,
    });
    types.sort_by(|a, b| (a.category, a.rank, &a.name).cmp(&(b.category, b.rank, &b.name)));
    types
}

/// The name a new source gets when its field is left empty: the type's name and one
/// more than the number of sources of that kind.
pub fn default_name(source_type: &SourceType, collection: &Collection) -> String {
    let count = collection
        .sources
        .iter()
        .filter(|s| matches!(&s.kind, SourceKind::Builtin { kind } if Some(kind) == source_type.kind.as_ref()))
        .count();
    format!("{} {}", source_type.name, count + 1)
}

/// A box of `native` size, scaled down to fit the canvas if it is larger, centred.
pub fn native_geometry(native: (f32, f32), canvas: Canvas) -> Option<ItemGeometry> {
    let (cw, ch) = (canvas.width as f32, canvas.height as f32);
    let k = 1.0_f32.min(cw / native.0).min(ch / native.1);
    let size = Size::try_new((native.0 * k).round(), (native.1 * k).round()).ok()?;
    ItemGeometry::try_new(Vec2::new(cw * 0.5, ch * 0.5)?, size).ok()
}

/// A reference's placement: its source's first placement anywhere, moved 40 px right
/// and down, or when the source isn't placed, a box of its own size (`source.sizes`),
/// else of its type's native size.
fn reference_geometry(
    source_id: &SourceId,
    source_type: &SourceType,
    collection: &Collection,
    sizes: &Sizes,
) -> Option<ItemGeometry> {
    let placed = collection
        .scenes
        .iter()
        .flat_map(|s| s.items.iter())
        .find(|i| i.source_id == *source_id)
        .and_then(|i| i.geometry);
    match placed {
        Some(mut geometry) => {
            let moved = Vec2::new(
                geometry.center.x.get() + 40.0,
                geometry.center.y.get() + 40.0,
            )?;
            geometry.center = moved;
            Some(geometry)
        }
        None => collection
            .source_size(sizes, source_id)
            .and_then(|(w, h)| native_geometry((w as f32, h as f32), collection.canvas))
            .or_else(|| placement(source_type, collection.canvas)),
    }
}

/// Where a new item of this type goes: its native box, or the whole canvas for a scene
/// or an audio-only source, which draw at canvas size or not at all.
fn placement(source_type: &SourceType, canvas: Canvas) -> Option<ItemGeometry> {
    source_type
        .native
        .and_then(|native| native_geometry(native, canvas))
        .or_else(|| ItemGeometry::covering_canvas(canvas.width, canvas.height).ok())
}

/// The sources of this type that can be placed again: every source of its kind, by
/// name. For the Scene type, the scenes that can be nested in `scene_id`: not itself,
/// and not one already nested there.
pub fn existing(
    source_type: &SourceType,
    collection: &Collection,
    scene_id: &SceneId,
) -> Vec<(String, String)> {
    let Some(kind) = &source_type.kind else {
        let nested: Vec<&SceneId> = collection
            .scene(scene_id)
            .into_iter()
            .flat_map(|s| s.items.iter())
            .filter_map(|i| match collection.source(&i.source_id).map(|s| &s.kind) {
                Some(SourceKind::Scene { scene_id }) => Some(scene_id),
                _ => None,
            })
            .collect();
        return collection
            .listed_scenes()
            .filter(|s| s.scene_id != *scene_id && !nested.contains(&&s.scene_id))
            .map(|s| (s.scene_id.to_string(), s.name.clone()))
            .collect();
    };
    collection
        .sources
        .iter()
        .filter(|s| matches!(&s.kind, SourceKind::Builtin { kind: k } if k == kind))
        .map(|s| (s.source_id.to_string(), s.name.clone()))
        .collect()
}

/// What the dialog's primary button does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Creates a new source of the type.
    New,
    /// Places an existing source again, or nests a scene: the ID from [`existing`].
    Existing(String),
}

/// The new IDs the edits use.
pub struct Ids {
    /// The new item's.
    pub item: ItemId,
    /// A new source's, when one is created.
    pub source: SourceId,
}

/// The edits that add `choice` of `source_type` to `scene_id`, named `name` when it
/// creates a source. An existing source not placed anywhere goes at its own size from
/// `sizes`, when the engine knows it.
pub fn add_edits(
    source_type: &SourceType,
    choice: &Choice,
    name: &str,
    scene_id: &SceneId,
    ids: Ids,
    collection: &Collection,
    sizes: &Sizes,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    let item = |source_id: SourceId, geometry: Option<ItemGeometry>| Edit::ItemAdd {
        scene_id: scene_id.clone(),
        item_id: ids.item.clone(),
        source_id,
        transform: None,
        geometry,
        opacity: None,
        blend: None,
    };
    match (choice, &source_type.kind) {
        (Choice::New, Some(kind)) if source_type.creatable => {
            let name = match name.trim() {
                "" => default_name(source_type, collection),
                given => given.to_owned(),
            };
            edits.push(Edit::SourceCreate {
                source_id: ids.source.clone(),
                name,
                kind: SourceKind::Builtin { kind: kind.clone() },
                settings: Settings::new(),
                duplicated_from: None,
            });
            edits.push(item(ids.source, placement(source_type, collection.canvas)));
        }
        (Choice::New, _) => {}
        (Choice::Existing(id), Some(_)) => {
            let source_id = SourceId::new(id.clone());
            let geometry = reference_geometry(&source_id, source_type, collection, sizes);
            edits.push(item(source_id, geometry));
        }
        (Choice::Existing(id), None) => {
            let shown = SceneId::new(id.clone());
            // A scene source already showing the scene is placed again; else one is made.
            let source_id =
                match collection.sources.iter().find(
                    |s| matches!(&s.kind, SourceKind::Scene { scene_id } if *scene_id == shown),
                ) {
                    Some(source) => source.source_id.clone(),
                    None => {
                        let name = collection
                            .scene(&shown)
                            .map_or_else(|| shown.to_string(), |s| s.name.clone());
                        edits.push(Edit::SourceCreate {
                            source_id: ids.source.clone(),
                            name,
                            kind: SourceKind::Scene { scene_id: shown },
                            settings: Settings::new(),
                            duplicated_from: None,
                        });
                        ids.source
                    }
                };
            edits.push(item(source_id, placement(source_type, collection.canvas)));
        }
    }
    edits
}

/// Opens the Add source dialog, provided as context by the shell.
#[derive(Clone, Copy)]
pub struct AddSourceDialog(RwSignal<Option<Request>>);

/// What the dialog was opened for.
#[derive(Clone, Debug, PartialEq)]
struct Request {
    /// The scene to add to.
    scene_id: SceneId,
    /// Whether it opens on the Scene type ("Add scene as source…").
    scene_type: bool,
}

impl AddSourceDialog {
    /// Closed.
    pub fn new() -> AddSourceDialog {
        AddSourceDialog(RwSignal::new(None))
    }

    /// Opens the dialog to add to `scene_id`, on the Scene type if `scene_type`.
    pub fn open(&self, scene_id: SceneId, scene_type: bool) {
        self.0.set(Some(Request {
            scene_id,
            scene_type,
        }));
    }
}

impl Default for AddSourceDialog {
    fn default() -> AddSourceDialog {
        AddSourceDialog::new()
    }
}

/// The dialog, drawn while it is open.
#[component]
pub fn AddSource() -> impl IntoView {
    let dialog = expect_context::<AddSourceDialog>();
    move || {
        dialog.0.get().map(|request| {
            view! { <AddSourceBody request=request close=move || dialog.0.set(None) /> }
        })
    }
}

#[component]
fn AddSourceBody(
    request: Request,
    close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let dock = use_context::<DockState>();
    let props = use_context::<PropertiesDialog>();
    let Request {
        scene_id,
        scene_type,
    } = request;
    let types = Memo::new(move |_| {
        studio.kinds.with(|k| {
            studio.collection.with(|c| {
                let placed: Vec<&str> = c
                    .iter()
                    .flat_map(|c| c.sources.iter())
                    .filter_map(|s| match &s.kind {
                        SourceKind::Builtin { kind } => Some(kind.as_str()),
                        _ => None,
                    })
                    .collect();
                catalogue(k.as_deref().unwrap_or_default(), &placed)
            })
        })
    });
    let first = types.with_untracked(|t| {
        t.iter()
            .find(|t| (t.kind.is_none()) == scene_type)
            .or(t.first())
            .map(SourceType::key)
            .unwrap_or_default()
    });
    let picked = RwSignal::new(first);
    // `None` until the user picks: Create new, or for Scene the first scene listed.
    let chosen = RwSignal::new(None::<Choice>);
    let name = RwSignal::new(String::new());

    let current = move || {
        let key = picked.get();
        types.with(|t| t.iter().find(|t| t.key() == key).cloned())
    };
    let existing_list = {
        let scene_id = scene_id.clone();
        move || {
            let Some(t) = current() else {
                return Vec::new();
            };
            studio.collection.with(|c| {
                c.as_ref()
                    .map(|c| existing(&t, c, &scene_id))
                    .unwrap_or_default()
            })
        }
    };
    let existing_list = Memo::new(move |_| existing_list());
    // What OK does now: the user's pick, else Create new, else the first existing one.
    let choice = move || {
        let can_new = current().is_some_and(|t| t.creatable);
        match chosen.get() {
            Some(Choice::New) if can_new => Some(Choice::New),
            Some(Choice::Existing(id))
                if existing_list.with(|l| l.iter().any(|(i, _)| *i == id)) =>
            {
                Some(Choice::Existing(id))
            }
            _ if can_new => Some(Choice::New),
            _ => existing_list.with(|l| l.first().map(|(id, _)| Choice::Existing(id.clone()))),
        }
    };
    let default_label = move || {
        let t = current()?;
        t.kind.as_ref()?;
        studio
            .collection
            .with(|c| c.as_ref().map(|c| default_name(&t, c)))
    };
    let where_label = {
        let scene_id = scene_id.clone();
        move || {
            let scene = studio.collection.with(|c| {
                c.as_ref()
                    .and_then(|c| c.scene(&scene_id))
                    .map_or_else(|| scene_id.to_string(), |s| s.name.clone())
            });
            format!("Into “{scene}”")
        }
    };

    let ok = {
        let scene_id = scene_id.clone();
        move || {
            let (Some(s), Some(t), Some(choice)) = (session.get(), current(), choice()) else {
                return;
            };
            let ids = Ids {
                item: ItemId::new(studio.fresh_id(&s, "item")),
                source: SourceId::new(studio.fresh_id(&s, "source")),
            };
            let item_id = ids.item.clone();
            let source_id = ids.source.clone();
            let edits = studio.collection.with_untracked(|c| {
                c.as_ref().map(|c| {
                    studio.sizes.with_untracked(|sizes| {
                        add_edits(&t, &choice, &name.get_untracked(), &scene_id, ids, c, sizes)
                    })
                })
            });
            let Some(edits) = edits.filter(|e| !e.is_empty()) else {
                return;
            };
            close();
            studio.select_item(Some(item_id));
            // A new source's properties open once the engine has it, as in OBS; the
            // dialog waits for the collection to catch up. A placement of an existing
            // source shows in the Inspector.
            match (&choice, props) {
                (Choice::New, Some(props)) if t.kind.is_some() => {
                    spawn_local(async move {
                        match apply(&s, edits, None).await {
                            Ok(_) => props.open(source_id),
                            Err(error) => studio.report(&error),
                        }
                    });
                }
                _ => {
                    studio.apply(&s, edits);
                    if let Some(dock) = dock {
                        dock.0.update(|d| d.show(PanelId::Inspector));
                    }
                }
            }
        }
    };
    let ok = StoredValue::new(ok);

    let type_list = move || {
        let types = types.get();
        let mut groups: Vec<(Category, Vec<SourceType>)> = Vec::new();
        for t in types {
            match groups.last_mut() {
                Some((category, list)) if *category == t.category => list.push(t),
                _ => groups.push((t.category, vec![t])),
            }
        }
        groups
            .into_iter()
            .map(|(category, list)| {
                let rows = list
                    .into_iter()
                    .map(|t| {
                        let key = t.key();
                        let creatable = t.creatable;
                        let on = {
                            let key = key.clone();
                            move || picked.with(|p| *p == key)
                        };
                        let pick = {
                            let key = key.clone();
                            move |_| {
                                picked.set(key.clone());
                                chosen.set(None);
                                name.set(String::new());
                            }
                        };
                        let quick = {
                            let key = key.clone();
                            move |_| {
                                if !creatable {
                                    return;
                                }
                                picked.set(key.clone());
                                chosen.set(Some(Choice::New));
                                name.set(String::new());
                                ok.with_value(|ok| ok());
                            }
                        };
                        view! {
                            <button
                                type="button"
                                role="option"
                                class="menuitem add-type"
                                class:on=on.clone()
                                aria-selected=move || if on() { "true" } else { "false" }
                                on:click=pick
                                on:dblclick=quick
                            >
                                <Icon name=t.icon />
                                <span class="add-type-name">{t.name.clone()}</span>
                            </button>
                        }
                    })
                    .collect_view();
                view! {
                    <div class="add-cat">{category.title()}</div>
                    {rows}
                }
            })
            .collect_view()
    };

    let detail = move || {
        let t = current()?;
        let is_scene = t.kind.is_none();
        let audio_only = t.native.is_none() && !is_scene;
        let lower = t.name.to_lowercase();
        let list = existing_list.get();
        let count = list.len();
        let existing_title = if is_scene {
            "Nest a scene".to_owned()
        } else {
            format!("Or add an existing {lower} as a reference")
        };
        let note = if is_scene {
            if count > 0 {
                "A nested scene is a reference: edit it in its own scene."
            } else {
                "No other scenes can be nested here."
            }
        } else if audio_only {
            "Audio only: it shows in the Mixer, not on the canvas."
        } else {
            ""
        };
        let rows = list
            .into_iter()
            .map(|(id, label)| {
                let on = {
                    let id = id.clone();
                    move || choice() == Some(Choice::Existing(id.clone()))
                };
                view! {
                    <label class="add-existing">
                        <input
                            type="radio"
                            name="addmode"
                            prop:checked=on
                            on:change=move |_| chosen.set(Some(Choice::Existing(id.clone())))
                        />
                        <span>{label}</span>
                    </label>
                }
            })
            .collect_view();
        Some(view! {
            <div class="add-head">
                <Icon name=t.icon size=22 />
                <div class="add-head-text">
                    <span class="add-type-title">{t.name.clone()}</span>
                    <span class="add-desc">{t.description.clone()}</span>
                </div>
            </div>
            {t.creatable.then(|| view! {
                <label class="add-new">
                    <input
                        type="radio"
                        name="addmode"
                        prop:checked=move || choice() == Some(Choice::New)
                        on:change=move |_| chosen.set(Some(Choice::New))
                    />
                    "Create new"
                </label>
                <input
                    class="add-name"
                    aria-label="Name"
                    placeholder=default_label
                    prop:value=move || name.get()
                    on:input=move |event| {
                        name.set(event_target_value(&event));
                        chosen.set(Some(Choice::New));
                    }
                    on:focus=move |_| chosen.set(Some(Choice::New))
                />
            })}
            {(count > 0).then(|| view! {
                <div class="add-existing-list">
                    <span class="add-existing-title">{existing_title}</span>
                    {rows}
                </div>
            })}
            {(!note.is_empty()).then(|| view! { <span class="add-note">{note}</span> })}
        })
    };

    let ok_label = move || {
        if choice() == Some(Choice::New) {
            "Create"
        } else {
            "Add"
        }
    };

    view! {
        <Modal label="Add source" class="add-source" cancel=close>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">"Add source"</h2>
                    <span class="modal-sub">{where_label}</span>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>
                    "×"
                </button>
            </div>
            <div class="add-body">
                <div class="add-types" role="listbox" aria-label="Source type">{type_list}</div>
                <div class="add-detail">{detail}</div>
            </div>
            <div class="modal-footer">
                <button type="button" class="secondary" on:click=move |_| close()>"Cancel"</button>
                <button
                    type="button"
                    class="primary"
                    data-primary="1"
                    disabled=move || choice().is_none()
                    on:click=move |_| ok.with_value(|ok| ok())
                >
                    {ok_label}
                </button>
            </div>
        </Modal>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{
        Crop, Finite32, ItemInfo, ItemTransform, PlacementForm, SceneInfo, SceneRole, SourceInfo,
    };

    fn kind(id: &str, name: &str) -> KindSummary {
        KindSummary {
            kind_id: id.into(),
            category: KindCategory::Source,
            name: name.into(),
            version: 1,
        }
    }

    fn engine_kinds() -> Vec<KindSummary> {
        vec![
            kind("youtube_chat", "YouTube chat"),
            kind("text", "Text"),
            kind("webcam", "Webcam"),
            kind("image", "Image"),
            kind("audio_input", "Audio input"),
            kind("display", "Display capture"),
            KindSummary {
                category: KindCategory::AudioFilter,
                ..kind("gain", "Gain")
            },
        ]
    }

    fn source(id: &str, name: &str, kind: SourceKind) -> SourceInfo {
        SourceInfo {
            source_id: id.into(),
            name: name.into(),
            kind,
            settings: Default::default(),
            duplicated_from: None,
            audio: None,
            filters: Default::default(),
            meta: Default::default(),
        }
    }

    fn item(id: &str, source_id: &str, geometry: Option<ItemGeometry>) -> ItemInfo {
        ItemInfo {
            item_id: id.into(),
            source_id: source_id.into(),
            placement: if geometry.is_some() {
                PlacementForm::Geometry
            } else {
                PlacementForm::Legacy
            },
            transform: ItemTransform::IDENTITY,
            geometry,
            crop: Crop::NONE,
            visible: true,
            effects: Vec::new(),
            opacity: Finite32::ONE,
            blend: Default::default(),
            transition: jstream_control::protocol::ItemTransition::default(),
        }
    }

    fn scene(id: &str, name: &str, items: Vec<ItemInfo>) -> SceneInfo {
        SceneInfo {
            overrides: Vec::new(),
            filters: Default::default(),
            variant_of: None,
            scene_id: id.into(),
            name: name.into(),
            role: SceneRole::Scene,
            folder: None,
            meta: Default::default(),
            items,
            transition_in: None,
            transition_out: None,
        }
    }

    fn collection() -> Collection {
        let cam_box = native_geometry((1280.0, 720.0), Canvas::default());
        Collection {
            sources: vec![
                source(
                    "cam",
                    "Brio",
                    SourceKind::Builtin {
                        kind: "webcam".into(),
                    },
                ),
                source(
                    "t1",
                    "Title",
                    SourceKind::Builtin {
                        kind: "text".into(),
                    },
                ),
                source(
                    "nest",
                    "Intro",
                    SourceKind::Scene {
                        scene_id: SceneId::new("intro"),
                    },
                ),
            ],
            scenes: vec![
                scene(
                    "main",
                    "Main",
                    vec![item("i1", "cam", cam_box), item("i2", "nest", None)],
                ),
                scene("intro", "Intro", Vec::new()),
                scene("brb", "BRB", Vec::new()),
            ],
            ..Collection::default()
        }
    }

    fn names(types: &[SourceType]) -> Vec<(&str, Category)> {
        types
            .iter()
            .map(|t| (t.name.as_str(), t.category))
            .collect()
    }

    #[test]
    fn the_types_follow_the_prototype_groups_and_order() {
        let types = catalogue(&engine_kinds(), &[]);
        assert_eq!(
            names(&types),
            [
                ("Display capture", Category::Capture),
                ("Video capture device", Category::Capture),
                ("Audio input", Category::Audio),
                ("Image", Category::MediaAndWeb),
                ("Text", Category::Generated),
                ("YouTube chat", Category::Other),
                ("Scene", Category::Scenes),
            ]
        );
        assert_eq!(types[1].description, "A webcam or capture card.");
        assert_eq!(types[1].icon, "webcam");
        assert_eq!(types[2].native, None, "audio input has no picture");
        assert!(!has_picture("audio_input"));
        assert!(has_picture("webcam"));
        assert!(
            has_picture("youtube_chat"),
            "an unknown kind is taken to draw"
        );
        // An engine that lists no kinds still offers Scene.
        assert_eq!(names(&catalogue(&[], &[])), [("Scene", Category::Scenes)]);
    }

    #[test]
    fn kinds_of_existing_sources_the_engine_does_not_list_can_be_placed_not_created() {
        let types = catalogue(&engine_kinds(), &["color", "text", "color", "gadget"]);
        let color = types.iter().find(|t| t.name == "Color").unwrap();
        let gadget = types.iter().find(|t| t.name == "gadget").unwrap();
        let text = types.iter().find(|t| t.name == "Text").unwrap();
        assert!(!color.creatable && !gadget.creatable && text.creatable);
        assert_eq!(
            (color.category, gadget.category),
            (Category::Generated, Category::Other)
        );
        assert_eq!(types.iter().filter(|t| t.name == "Text").count(), 1);
        let ids = Ids {
            item: ItemId::new("item-1"),
            source: SourceId::new("source-1"),
        };
        let edits = add_edits(
            color,
            &Choice::New,
            "",
            &SceneId::new("main"),
            ids,
            &collection(),
            &Sizes::new(),
        );
        assert!(edits.is_empty(), "an unlisted kind can't be created");
    }

    #[test]
    fn new_sources_are_named_after_their_type_and_count() {
        let types = catalogue(&engine_kinds(), &[]);
        let c = collection();
        let webcam = types
            .iter()
            .find(|t| t.name == "Video capture device")
            .unwrap();
        let image = types.iter().find(|t| t.name == "Image").unwrap();
        assert_eq!(default_name(webcam, &c), "Video capture device 2");
        assert_eq!(default_name(image, &c), "Image 1");
    }

    #[test]
    fn native_boxes_are_centred_and_fit_the_canvas() {
        let canvas = Canvas::default();
        let text = native_geometry((800.0, 90.0), canvas).unwrap();
        assert_eq!((text.center.x.get(), text.center.y.get()), (960.0, 540.0));
        assert_eq!(
            (text.size.width.get(), text.size.height.get()),
            (800.0, 90.0)
        );
        // A display on a 1280 × 720 canvas is scaled down to fit it.
        let small = Canvas {
            width: 1280,
            height: 720,
        };
        let display = native_geometry((1920.0, 1080.0), small).unwrap();
        assert_eq!(
            (display.size.width.get(), display.size.height.get()),
            (1280.0, 720.0)
        );
        assert_eq!(
            (display.center.x.get(), display.center.y.get()),
            (640.0, 360.0)
        );
    }

    #[test]
    fn creating_a_source_sends_its_creation_and_native_placement() {
        let types = catalogue(&engine_kinds(), &[]);
        let image = types.iter().find(|t| t.name == "Image").unwrap();
        let ids = Ids {
            item: ItemId::new("item-1"),
            source: SourceId::new("source-1"),
        };
        let edits = add_edits(
            image,
            &Choice::New,
            "  ",
            &SceneId::new("main"),
            ids,
            &collection(),
            &Sizes::new(),
        );
        let [
            Edit::SourceCreate { name, kind, .. },
            Edit::ItemAdd {
                scene_id,
                source_id,
                geometry: Some(g),
                ..
            },
        ] = edits.as_slice()
        else {
            panic!("source.create then item.add: {edits:?}");
        };
        assert_eq!(name, "Image 1");
        assert_eq!(
            kind,
            &SourceKind::Builtin {
                kind: "image".into()
            }
        );
        assert_eq!(
            (scene_id.as_str(), source_id.as_str()),
            ("main", "source-1")
        );
        assert_eq!((g.size.width.get(), g.size.height.get()), (400.0, 400.0));
    }

    #[test]
    fn an_existing_source_is_placed_again_offset_from_its_first_placement() {
        let types = catalogue(&engine_kinds(), &[]);
        let webcam = types
            .iter()
            .find(|t| t.name == "Video capture device")
            .unwrap();
        let c = collection();
        assert_eq!(
            existing(webcam, &c, &SceneId::new("brb")),
            [("cam".to_owned(), "Brio".to_owned())]
        );
        let ids = Ids {
            item: ItemId::new("item-1"),
            source: SourceId::new("unused"),
        };
        let edits = add_edits(
            webcam,
            &Choice::Existing("cam".into()),
            "",
            &SceneId::new("brb"),
            ids,
            &c,
            &Sizes::new(),
        );
        let [
            Edit::ItemAdd {
                source_id,
                geometry: Some(g),
                ..
            },
        ] = edits.as_slice()
        else {
            panic!("one item.add: {edits:?}");
        };
        assert_eq!(source_id.as_str(), "cam");
        assert_eq!((g.center.x.get(), g.center.y.get()), (1000.0, 580.0));
    }

    #[test]
    fn an_unplaced_source_goes_at_its_own_size_when_the_engine_knows_it() {
        let types = catalogue(&engine_kinds(), &[]);
        let webcam = types
            .iter()
            .find(|t| t.name == "Video capture device")
            .unwrap();
        let mut c = collection();
        for scene in &mut c.scenes {
            scene.items.retain(|i| i.source_id.as_str() != "cam");
        }
        let place = |sizes: &Sizes| {
            let ids = Ids {
                item: ItemId::new("item-1"),
                source: SourceId::new("unused"),
            };
            let edits = add_edits(
                webcam,
                &Choice::Existing("cam".into()),
                "",
                &SceneId::new("brb"),
                ids,
                &c,
                sizes,
            );
            let [
                Edit::ItemAdd {
                    geometry: Some(g), ..
                },
            ] = edits.as_slice()
            else {
                panic!("one item.add: {edits:?}");
            };
            (g.size.width.get(), g.size.height.get())
        };
        let own = Sizes::from([(SourceId::new("cam"), (640, 480))]);
        assert_eq!(place(&own), (640.0, 480.0));
        // Larger than the canvas, it is scaled down to fit.
        let big = Sizes::from([(SourceId::new("cam"), (3840, 2160))]);
        assert_eq!(place(&big), (1920.0, 1080.0));
        // Unknown, it takes its type's native size.
        assert_eq!(place(&Sizes::new()), webcam.native.unwrap());
    }

    #[test]
    fn a_scene_nests_once_and_reuses_its_scene_source() {
        let types = catalogue(&engine_kinds(), &[]);
        let scene_type = types.last().unwrap();
        let c = collection();
        // Main already nests Intro, and can't nest itself.
        assert_eq!(
            existing(scene_type, &c, &SceneId::new("main")),
            [("brb".to_owned(), "BRB".to_owned())]
        );
        let ids = || Ids {
            item: ItemId::new("item-1"),
            source: SourceId::new("source-1"),
        };
        // Intro has a scene source: BRB places it.
        let edits = add_edits(
            scene_type,
            &Choice::Existing("intro".into()),
            "",
            &SceneId::new("brb"),
            ids(),
            &c,
            &Sizes::new(),
        );
        assert!(
            matches!(edits.as_slice(), [Edit::ItemAdd { source_id, .. }] if source_id.as_str() == "nest")
        );
        // BRB has none: one is made, named after the scene.
        let edits = add_edits(
            scene_type,
            &Choice::Existing("brb".into()),
            "",
            &SceneId::new("main"),
            ids(),
            &c,
            &Sizes::new(),
        );
        assert!(
            matches!(edits.as_slice(), [Edit::SourceCreate { name, .. }, Edit::ItemAdd { .. }] if name == "BRB"),
            "{edits:?}"
        );
        // Scene can't be created new.
        assert!(
            add_edits(
                scene_type,
                &Choice::New,
                "",
                &SceneId::new("main"),
                ids(),
                &c,
                &Sizes::new(),
            )
            .is_empty()
        );
    }
}
