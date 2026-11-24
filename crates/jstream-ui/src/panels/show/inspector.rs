//! The Inspector's Show page: what the Show panel's Edit view has selected (a program,
//! overlay or audio clip, a bookmark, a loop, a keyframe marker or a cue), as a grid of
//! fields, over the show's own settings. The Inspector shows it ahead of its usual page
//! while [`ShowPanel::selected`] names something in the open show.
//!
//! Every field's change is one [`ShowPanel::set_show`], so one undo step, made by an edit
//! in [`super::inspect_edits`]. What the page lists and how it labels it are the plain
//! functions here, tested natively.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#inspector

use super::inspect_edits::{self as ie, AtEnd};
use super::{ShowPanel, edits, wire};
use crate::panels::inspector::{Fold, HotkeyRow};
use crate::panels::number::NumberField;
use crate::store::{Collection, Studio};
use jstream_control::protocol::{
    Action, LoopLeave, SceneId, ShowEnd, ShowInfo, SourceId, TransitionPresetId,
};
use leptos::prelude::*;

/// What a selection in the show is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A clip on the program track.
    Program,
    /// A clip on an overlay track.
    Overlay,
    /// A clip on an audio track.
    Audio,
    /// A bookmark.
    Bookmark,
    /// A loop.
    Loop,
    /// A keyframe marker.
    Keyframe,
    /// A cue on a sequences track.
    Cue,
}

impl Kind {
    /// The page's name beside the Inspector's title.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Program => "Show clip",
            Kind::Overlay => "Show overlay",
            Kind::Audio => "Show audio",
            Kind::Bookmark => "Show bookmark",
            Kind::Loop => "Show loop",
            Kind::Keyframe => "Show keyframe",
            Kind::Cue => "Show cue",
        }
    }

    /// The Delete button's text.
    fn delete(self) -> &'static str {
        match self {
            Kind::Program | Kind::Overlay | Kind::Audio => "Delete clip",
            Kind::Bookmark => "Delete bookmark",
            Kind::Loop => "Delete loop",
            Kind::Keyframe => "Delete keyframe marker",
            Kind::Cue => "Delete cue",
        }
    }
}

/// What `id` is in `show`, if the show holds it.
pub fn kind_of(show: &ShowInfo, id: &str) -> Option<Kind> {
    if show.program.iter().any(|c| c.clip_id == id) {
        Some(Kind::Program)
    } else if show
        .overlays
        .iter()
        .flat_map(|t| &t.clips)
        .any(|c| c.clip_id == id)
    {
        Some(Kind::Overlay)
    } else if show
        .audio
        .iter()
        .flat_map(|t| &t.clips)
        .any(|c| c.clip_id == id)
    {
        Some(Kind::Audio)
    } else if show.bookmarks.iter().any(|b| b.bookmark_id == id) {
        Some(Kind::Bookmark)
    } else if show.loops.iter().any(|l| l.loop_id == id) {
        Some(Kind::Loop)
    } else if show.keyframes.iter().any(|m| m.marker_id == id) {
        Some(Kind::Keyframe)
    } else if show
        .sequences
        .iter()
        .flat_map(|t| &t.cues)
        .any(|c| c.cue_id == id)
    {
        Some(Kind::Cue)
    } else {
        None
    }
}

/// The selection's name, for the page's header: a clip's scene or source, a bookmark's
/// or loop's name, a marker's keyframe, a cue's action.
pub fn name_of(show: &ShowInfo, collection: &Collection, id: &str) -> String {
    let scene = |scene: &SceneId| {
        collection
            .scene(scene)
            .map_or_else(|| "(missing)".to_owned(), |s| s.name.clone())
    };
    if let Some(c) = show.program.iter().find(|c| c.clip_id == id) {
        return scene(&c.scene_id);
    }
    if let Some(c) = show
        .overlays
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| c.clip_id == id)
    {
        return scene(&c.scene_id);
    }
    if let Some(c) = show
        .audio
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| c.clip_id == id)
    {
        return collection
            .sources
            .iter()
            .find(|s| s.source_id == c.source_id)
            .map_or_else(|| "(missing)".to_owned(), |s| s.name.clone());
    }
    if let Some(b) = show.bookmark(id) {
        return b.name.clone();
    }
    if let Some(l) = show.loops.iter().find(|l| l.loop_id == id) {
        return l.name.clone();
    }
    if let Some(m) = show.keyframes.iter().find(|m| m.marker_id == id) {
        return m
            .keyframe_id
            .as_ref()
            .map_or_else(|| "Scene's own".to_owned(), scene);
    }
    show.sequences
        .iter()
        .flat_map(|t| &t.cues)
        .find(|c| c.cue_id == id)
        .and_then(|c| c.action())
        .map(|a| crate::panels::events::action_text(collection, &a))
        .unwrap_or_default()
}

/// A time as a field shows it: `m:ss`, `h:mm:ss` from an hour, with tenths when it is
/// not a whole second (`1:30.5`).
pub fn clock_text(ms: u32) -> String {
    let whole = super::time::clock(ms);
    match (ms % 1000) / 100 {
        0 => whole,
        tenths => format!("{whole}.{tenths}"),
    }
}

/// A time typed into a field: `1:30`, `1:30.5`, `1:02:03`, or plain seconds (`90`,
/// `2.5`). `None` for anything else.
pub fn parse_clock(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let (last, whole) = parts.split_last()?;
    let seconds: f64 = last.replace(',', ".").parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 || (!whole.is_empty() && seconds >= 60.0) {
        return None;
    }
    let mut total = 0_f64;
    for (n, part) in whole.iter().enumerate() {
        let v: u32 = part.parse().ok()?;
        if n > 0 && v >= 60 {
            return None;
        }
        total = total * 60.0 + f64::from(v);
    }
    let ms = ((total * 60.0 + seconds) * 1000.0).round();
    (ms <= f64::from(u32::MAX)).then_some(ms as u32)
}

/// A bookmark in a program clip, as the clip's page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipBookmark {
    /// Its ID.
    pub id: String,
    /// Its name and its go-to key: "Close-up · F2".
    pub label: String,
    /// Where it is in the clip and how a jump arrives: "at 1:30 · arrive: Move 0.8 s".
    pub detail: String,
}

/// The bookmarks in program clip `clip`: after its start (or at it, for the first clip)
/// up to and with its end. `key` gives a bookmark's go-to key, `badge` a preset's name.
pub fn clip_bookmarks(
    show: &ShowInfo,
    clip: &str,
    key: impl Fn(&str) -> Option<String>,
    badge: impl Fn(Option<&TransitionPresetId>) -> String,
) -> Vec<ClipBookmark> {
    let Some((start, end)) = ie::program_span(show, clip) else {
        return Vec::new();
    };
    show.bookmarks
        .iter()
        .filter(|b| (b.at_ms > start || (start == 0 && b.at_ms == 0)) && b.at_ms <= end)
        .map(|b| {
            let name = if b.name.is_empty() {
                "Bookmark"
            } else {
                b.name.as_str()
            };
            let label = match key(&b.bookmark_id) {
                Some(k) => format!("{name} · {k}"),
                None => name.to_owned(),
            };
            let mut detail = format!(
                "at {} · arrive: {}",
                clock_text(b.at_ms - start),
                badge(b.preset_id.as_ref())
            );
            if !b.pausing {
                detail.push_str(" · plays through");
            }
            ClipBookmark {
                id: b.bookmark_id.clone(),
                label,
                detail,
            }
        })
        .collect()
}

/// Bookmark `id`'s number for "Go to bookmark N", 1 to 8 in show order, if it has one.
pub fn bookmark_number(show: &ShowInfo, id: &str) -> Option<u8> {
    let n = show.bookmarks.iter().position(|b| b.bookmark_id == id)?;
    u8::try_from(n + 1)
        .ok()
        .filter(|&n| n <= jstream_control::protocol::SHOW_NUMBERED_BOOKMARKS)
}

/// The scene on program at `at_ms`: the clip it falls in, or the last one past the end.
pub fn scene_at(show: &ShowInfo, at_ms: u32) -> Option<SceneId> {
    show.program
        .iter()
        .zip(show.program_starts())
        .take_while(|(_, start)| *start <= at_ms)
        .last()
        .map(|(c, _)| c.scene_id.clone())
}

/// A transition picker's options, `(value, label)`: `default` first with the empty
/// value, then every preset by its badge ("Move 0.8 s").
pub fn preset_options(collection: &Collection, default: &str) -> Vec<(String, String)> {
    std::iter::once((String::new(), default.to_owned()))
        .chain(collection.transitions.presets.iter().map(|p| {
            (
                p.preset_id.to_string(),
                wire::preset_badge(collection, Some(&p.preset_id)),
            )
        }))
        .collect()
}

/// The collection's default preset's badge.
fn collection_default(collection: &Collection) -> String {
    let id = collection.transitions.default.clone();
    wire::preset_badge(collection, Some(&id))
}

/// The "default" option of a bookmark's or loop's transition: the show's preset, else
/// the collection's: "Default (Fade 0.5 s)".
pub fn show_default_label(collection: &Collection, show: &ShowInfo) -> String {
    let name = match &show.preset_id {
        Some(p) => wire::preset_badge(collection, Some(p)),
        None => collection_default(collection),
    };
    format!("Default ({name})")
}

/// The "default" option of a transition that falls back on the collection's default.
pub fn collection_default_label(collection: &Collection) -> String {
    format!("Default ({})", collection_default(collection))
}

/// The scenes a clip can put on program, `(ID, name)`: every scene that is not a
/// keyframe scene, with `current` too if the collection no longer holds it.
pub fn scene_options(collection: &Collection, current: Option<&SceneId>) -> Vec<(String, String)> {
    let mut options: Vec<(String, String)> = collection
        .scenes
        .iter()
        .filter(|s| s.variant_of.is_none())
        .map(|s| (s.scene_id.to_string(), s.name.clone()))
        .collect();
    if let Some(current) = current
        && !options.iter().any(|(id, _)| id == current.as_str())
    {
        options.insert(0, (current.to_string(), "(missing)".to_owned()));
    }
    options
}

/// The sources an audio clip can play, `(ID, name)`, with `current` too if the
/// collection no longer holds it.
pub fn source_options(collection: &Collection, current: &SourceId) -> Vec<(String, String)> {
    let mut options: Vec<(String, String)> = collection
        .sources
        .iter()
        .map(|s| (s.source_id.to_string(), s.name.clone()))
        .collect();
    if !options.iter().any(|(id, _)| id == current.as_str()) {
        options.insert(0, (current.to_string(), "(missing)".to_owned()));
    }
    options
}

/// A keyframe marker's options, `(value, label)`: "Scene's own" (the empty value), then
/// the keyframes of `scene` (what is on program at the marker), and `current` too if it
/// is a keyframe of another scene.
pub fn keyframe_options(
    collection: &Collection,
    scene: Option<&SceneId>,
    current: Option<&SceneId>,
) -> Vec<(String, String)> {
    let mut options = vec![(String::new(), "Scene's own".to_owned())];
    if let Some(scene) = scene {
        options.extend(
            collection
                .keyframes_of(scene)
                .map(|k| (k.scene_id.to_string(), k.name.clone())),
        );
    }
    if let Some(current) = current
        && !options.iter().any(|(id, _)| id == current.as_str())
    {
        let name = collection
            .scene(current)
            .map_or_else(|| "(missing)".to_owned(), |s| s.name.clone());
        options.push((current.to_string(), name));
    }
    options
}

/// The End of show options, `(value, label)`: Stop, Loop, then a Go to for each bookmark.
pub fn end_options(show: &ShowInfo) -> Vec<(String, String)> {
    let mut options = vec![
        ("stop".to_owned(), "Stop".to_owned()),
        ("loop".to_owned(), "Loop to the start".to_owned()),
    ];
    options.extend(show.bookmarks.iter().map(|b| {
        (
            format!("go:{}", b.bookmark_id),
            format!(
                "Go to {}",
                if b.name.is_empty() {
                    &b.bookmark_id
                } else {
                    &b.name
                }
            ),
        )
    }));
    options
}

/// The End of show picker's value for `end`.
pub fn end_key(end: &ShowEnd) -> String {
    match end {
        ShowEnd::Loop => "loop".to_owned(),
        ShowEnd::GoTo { bookmark_id } => format!("go:{bookmark_id}"),
        _ => "stop".to_owned(),
    }
}

/// The show end a picker's value names.
pub fn end_from_key(key: &str) -> ShowEnd {
    match key {
        "loop" => ShowEnd::Loop,
        key => key
            .strip_prefix("go:")
            .map_or(ShowEnd::Stop, |id| ShowEnd::GoTo {
                bookmark_id: id.to_owned(),
            }),
    }
}

/// The picker's value for how a loop is left.
fn leave_key(leave: LoopLeave) -> &'static str {
    match leave {
        LoopLeave::FinishPass => "finish",
        _ => "once",
    }
}

/// An empty value as `None`, else a preset ID.
fn preset_of(value: &str) -> Option<TransitionPresetId> {
    (!value.is_empty()).then(|| TransitionPresetId::new(value.to_owned()))
}

/// The labels of the "Go to bookmark N" rows.
const GO_TO_LABELS: [&str; 8] = [
    "Go to bookmark 1",
    "Go to bookmark 2",
    "Go to bookmark 3",
    "Go to bookmark 4",
    "Go to bookmark 5",
    "Go to bookmark 6",
    "Go to bookmark 7",
    "Go to bookmark 8",
];

/// What the page works on: the panel, the studio, and the show and selection by ID.
#[derive(Clone, Copy)]
struct Page {
    panel: ShowPanel,
    studio: Studio,
    show_id: StoredValue<String>,
    id: StoredValue<String>,
    /// The show as the collection holds it now.
    show: Memo<Option<ShowInfo>>,
}

impl Page {
    /// Changes the show with `f` and sends it as one `show.set`, when `f` says it changed.
    fn apply(self, f: impl FnOnce(&mut ShowInfo) -> bool) -> bool {
        let Some(mut show) = self.show.get_untracked() else {
            return false;
        };
        if !f(&mut show) {
            return false;
        }
        let id = self.show_id.get_value();
        self.panel.set_show(&id, move |s| *s = show);
        true
    }

    /// The selection's ID.
    fn id(self) -> String {
        self.id.get_value()
    }

    /// Reads the show and the collection, or `default` while either is missing.
    fn read<T>(self, default: T, f: impl FnOnce(&ShowInfo, &Collection) -> T) -> T {
        self.show.with(|s| {
            self.studio.collection.with(|c| match (s, c) {
                (Some(s), Some(c)) => f(s, c),
                _ => default,
            })
        })
    }
}

/// A picker row.
fn select_row(
    label: &'static str,
    options: impl Fn() -> Vec<(String, String)> + Send + Sync + 'static,
    value: impl Fn() -> String + Send + Sync + 'static,
    change: impl Fn(String) + 'static,
) -> AnyView {
    view! {
        <label class="select-field">
            <span class="field-label">{label}</span>
            <select aria-label=label on:change=move |ev| change(event_target_value(&ev))>
                {move || {
                    let now = value();
                    options()
                        .into_iter()
                        .map(|(v, name)| {
                            let selected = v == now;
                            view! { <option value=v selected=selected>{name}</option> }
                        })
                        .collect_view()
                }}
            </select>
        </label>
    }
    .into_any()
}

/// A text row that sends on change; a refused value shows the committed one again.
fn text_row(
    label: &'static str,
    placeholder: &'static str,
    value: impl Fn() -> String + Send + Sync + 'static,
    commit: impl Fn(String) -> bool + 'static,
) -> AnyView {
    let again = RwSignal::new(0_u32);
    view! {
        <label class="select-field text-field">
            <span class="field-label">{label}</span>
            <input
                type="text"
                aria-label=label
                placeholder=placeholder
                prop:value=move || {
                    again.track();
                    value()
                }
                on:change=move |ev| {
                    if !commit(event_target_value(&ev)) {
                        again.update(|n| *n += 1);
                    }
                }
            />
        </label>
    }
    .into_any()
}

/// A time row (`m:ss`). With `optional`, an empty field sends `None`.
fn clock_row(
    label: &'static str,
    placeholder: &'static str,
    value: impl Fn() -> Option<u32> + Send + Sync + 'static,
    commit: impl Fn(Option<u32>) -> bool + 'static,
) -> AnyView {
    text_row(
        label,
        placeholder,
        move || value().map(clock_text).unwrap_or_default(),
        move |text| {
            if text.trim().is_empty() {
                return !placeholder.is_empty() && commit(None);
            }
            parse_clock(&text).is_some_and(|ms| commit(Some(ms)))
        },
    )
}

/// A row of seconds, from a value in milliseconds.
fn seconds_row(
    label: &'static str,
    max_s: f64,
    value: impl Fn() -> Option<u32> + Send + Sync + 'static,
    commit: impl Fn(u32) -> bool + Copy + Send + Sync + 'static,
) -> AnyView {
    view! {
        <NumberField
            label=label
            unit="s"
            value=Signal::derive(move || value().map(|ms| f64::from(ms) / 1000.0))
            step=0.1
            decimals=1
            min=0.0
            max=max_s
            send=move |s| commit((s * 1000.0).round() as u32)
        />
    }
    .into_any()
}

/// A read-only row.
fn read_row(label: &'static str, value: impl Fn() -> String + Send + Sync + 'static) -> AnyView {
    view! {
        <div class="show-row show-read">
            <span class="key">{label}</span>
            <span class="value">{value}</span>
        </div>
    }
    .into_any()
}

/// The Show page for `id` in show `show_id`, which is a `kind`.
#[component]
pub fn ShowPage(
    /// The open show, by ID.
    show_id: String,
    /// The selection, by its ID in the show.
    id: String,
    /// What the selection is.
    kind: Kind,
) -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let studio = expect_context::<Studio>();
    let show_key = StoredValue::new(show_id);
    let show = Memo::new(move |_| {
        studio.collection.with(|c| {
            c.as_ref().and_then(|c| {
                c.shows
                    .iter()
                    .find(|s| show_key.with_value(|k| s.show_id.as_str() == k))
                    .cloned()
            })
        })
    });
    let page = Page {
        panel,
        studio,
        show_id: show_key,
        id: StoredValue::new(id),
        show,
    };
    let locked = move || {
        panel
            .locked
            .with(|l| show_key.with_value(|id| l.contains(id)))
    };
    let body = match kind {
        Kind::Program => program_page(page),
        Kind::Overlay => overlay_page(page),
        Kind::Audio => audio_page(page),
        Kind::Bookmark => bookmark_page(page),
        Kind::Loop => loop_page(page),
        Kind::Keyframe => keyframe_page(page),
        Kind::Cue => cue_page(page),
    };
    let delete = move |_| {
        let id = page.id();
        if page.apply(|s| edits::remove(s, &id)) {
            panel.selected.set(None);
        }
    };
    let name = move || page.read(String::new(), |s, c| name_of(s, c, &page.id()));
    view! {
        <div class="inspector show-inspector">
            <div class="inspector-head">
                <span class="source-title" title=name>{name}</span>
                <span class="kind-tag">{kind.title()}</span>
            </div>
            <fieldset class="show-fields" disabled=locked>
                {body}
                <div class="show-actions">
                    <button type="button" class="show-delete" on:click=delete>{kind.delete()}</button>
                    <button type="button" on:click=move |_| panel.selected.set(None)>"Done"</button>
                </div>
                {show_settings(page)}
            </fieldset>
            {move || locked().then(|| view! {
                <p class="note">"The show is locked: unlock it in the Show panel to edit."</p>
            })}
        </div>
    }
}

/// A program clip: its scene, cut, length and end, the bookmarks in it, and Continue.
fn program_page(page: Page) -> AnyView {
    let clip = Memo::new(move |_| {
        page.show.with(|s| {
            s.as_ref()
                .and_then(|s| s.program.iter().find(|c| c.clip_id == page.id()).cloned())
        })
    });
    let scene = move || clip.with(|c| c.as_ref().map(|c| c.scene_id.clone()));
    let scene_name = move || {
        let scene = scene();
        page.read(String::new(), |_, c| {
            scene
                .and_then(|id| c.scene(&id).map(|s| s.name.clone()))
                .unwrap_or_default()
        })
    };
    let timed = move || {
        let scene = scene();
        page.read(false, |_, c| scene.is_some_and(|id| wire::is_timed(c, &id)))
    };
    let scene_row = select_row(
        "Scene",
        move || page.read(Vec::new(), |_, c| scene_options(c, scene().as_ref())),
        move || scene().map(|s| s.to_string()).unwrap_or_default(),
        move |v| {
            page.apply(|s| ie::set_scene(s, &page.id(), SceneId::new(v)));
        },
    );
    let cut_row = select_row(
        "Transition in",
        move || page.read(Vec::new(), |_, c| preset_options(c, "Default")),
        move || {
            clip.with(|c| {
                c.as_ref()
                    .and_then(|c| c.preset_id.as_ref().map(ToString::to_string))
                    .unwrap_or_default()
            })
        },
        move |v| {
            page.apply(|s| edits::set_cut(s, &page.id(), preset_of(&v)));
        },
    );
    let length_row = clock_row(
        "Timed length",
        "",
        move || clip.with(|c| c.as_ref().map(|c| c.length_ms)),
        move |ms| ms.is_some_and(|ms| page.apply(|s| ie::set_program_length(s, &page.id(), ms))),
    );
    let end_row = select_row(
        "At the end",
        || {
            AtEnd::ALL
                .iter()
                .map(|a| (a.key().to_owned(), a.label().to_owned()))
                .collect()
        },
        move || {
            page.show.with(|s| {
                s.as_ref()
                    .map_or(AtEnd::Nothing, |s| ie::at_end(s, &page.id()))
                    .key()
                    .to_owned()
            })
        },
        move |v| {
            if let Some(to) = AtEnd::from_key(&v) {
                let name = scene_name();
                page.apply(|s| ie::set_at_end(s, &page.id(), to, &name));
            }
        },
    );
    let rows = move || {
        page.read(Vec::new(), |s, c| {
            clip_bookmarks(
                s,
                &page.id(),
                |b| {
                    wire::keys_for(
                        c,
                        &Action::ShowGoTo {
                            show_id: s.show_id.clone(),
                            bookmark_id: b.to_owned(),
                        },
                    )
                    .into_iter()
                    .next()
                },
                |p| match p {
                    Some(p) => wire::preset_badge(c, Some(p)),
                    None => match &s.preset_id {
                        Some(p) => wire::preset_badge(c, Some(p)),
                        None => collection_default(c),
                    },
                },
            )
        })
    };
    let bookmark_row = move |b: ClipBookmark| {
        let (pick, remove) = (b.id.clone(), b.id.clone());
        let title = format!("Remove {}", b.label);
        view! {
            <div class="show-row">
                <span class="key">{b.label.clone()}</span>
                <span class="show-box">
                    <button type="button" class="show-box-text" title="Inspect this bookmark"
                        on:click=move |_| page.panel.selected.set(Some(pick.clone()))>
                        {b.detail.clone()}
                    </button>
                    <button type="button" class="icon-button" title=title.clone() aria-label=title
                        on:click=move |_| {
                            page.apply(|s| edits::remove(s, &remove));
                        }>"×"</button>
                </span>
            </div>
        }
    };
    view! {
        <Fold title="Clip" open=true>
            <div class="field-grid">
                {scene_row}
                {cut_row}
                {length_row}
                {end_row}
            </div>
            {move || timed().then(|| view! {
                <p class="note">"A video's clip runs its video length; a shorter length cuts it."</p>
            })}
        </Fold>
        <Fold title="Bookmarks in this clip" open=true>
            <div class="show-rows">
                {move || {
                    let rows = rows();
                    if rows.is_empty() {
                        view! { <p class="note">"None. The Bookmark tool adds one on the timeline."</p> }.into_any()
                    } else {
                        rows.into_iter().map(bookmark_row).collect_view().into_any()
                    }
                }}
            </div>
            {read_row("While paused", || "Scene keeps running".to_owned())}
        </Fold>
        <Fold title="Continue is bound to" open=true>
            <HotkeyRow label="Keys" action=Action::ShowContinue />
            <p class="note">"Continue plays on from a pause, or to the next pausing bookmark."</p>
        </Fold>
    }
    .into_any()
}

/// Start, length and fades, for an overlay or audio clip.
fn span_rows(page: Page) -> AnyView {
    let span = Memo::new(move |_| {
        page.show
            .with(|s| s.as_ref().and_then(|s| ie::span_of(s, &page.id())))
    });
    let fades = Memo::new(move |_| {
        page.show.with(|s| {
            let s = s.as_ref()?;
            let id = page.id();
            s.overlays
                .iter()
                .flat_map(|t| &t.clips)
                .find(|c| c.clip_id == id)
                .map(|c| (c.fade_in_ms, c.fade_out_ms))
                .or_else(|| {
                    s.audio
                        .iter()
                        .flat_map(|t| &t.clips)
                        .find(|c| c.clip_id == id)
                        .map(|c| (c.fade_in_ms, c.fade_out_ms))
                })
        })
    });
    let max_fade = f64::from(jstream_control::protocol::MAX_SHOW_FADE_MS) / 1000.0;
    view! {
        {clock_row("Start", "", move || span.get().map(|s| s.0),
            move |ms| ms.is_some_and(|ms| page.apply(|s| ie::set_clip_start(s, &page.id(), ms))))}
        {clock_row("Length", "", move || span.get().map(|s| s.1),
            move |ms| ms.is_some_and(|ms| page.apply(|s| ie::set_clip_length(s, &page.id(), ms))))}
        {seconds_row("Fade in", max_fade, move || fades.get().map(|f| f.0), move |ms| {
            let out = fades.get_untracked().map_or(0, |f| f.1);
            page.apply(|s| edits::set_fades(s, &page.id(), (ms, out)))
        })}
        {seconds_row("Fade out", max_fade, move || fades.get().map(|f| f.1), move |ms| {
            let fade_in = fades.get_untracked().map_or(0, |f| f.0);
            page.apply(|s| edits::set_fades(s, &page.id(), (fade_in, ms)))
        })}
    }
    .into_any()
}

/// An overlay clip: its scene, start, length and fades.
fn overlay_page(page: Page) -> AnyView {
    let scene = Memo::new(move |_| {
        page.show.with(|s| {
            s.as_ref().and_then(|s| {
                s.overlays
                    .iter()
                    .flat_map(|t| &t.clips)
                    .find(|c| c.clip_id == page.id())
                    .map(|c| c.scene_id.clone())
            })
        })
    });
    let scene_row = select_row(
        "Scene",
        move || page.read(Vec::new(), |_, c| scene_options(c, scene.get().as_ref())),
        move || scene.get().map(|s| s.to_string()).unwrap_or_default(),
        move |v| {
            page.apply(|s| ie::set_scene(s, &page.id(), SceneId::new(v)));
        },
    );
    view! {
        <Fold title="Overlay clip" open=true>
            <div class="field-grid">
                {scene_row}
                {span_rows(page)}
            </div>
            <p class="note">"An overlay plays over program from its start, fading in and out."</p>
        </Fold>
    }
    .into_any()
}

/// An audio clip: its source, start, length, fades and level.
fn audio_page(page: Page) -> AnyView {
    let clip = Memo::new(move |_| {
        page.show.with(|s| {
            s.as_ref().and_then(|s| {
                s.audio
                    .iter()
                    .flat_map(|t| &t.clips)
                    .find(|c| c.clip_id == page.id())
                    .map(|c| (c.source_id.clone(), c.gain_db.get()))
            })
        })
    });
    let source_row = select_row(
        "Source",
        move || {
            let current = clip.get().map(|c| c.0);
            page.read(Vec::new(), |_, c| {
                current.map_or_else(Vec::new, |cur| source_options(c, &cur))
            })
        },
        move || clip.get().map(|c| c.0.to_string()).unwrap_or_default(),
        move |v| {
            page.apply(|s| ie::set_source(s, &page.id(), SourceId::new(v)));
        },
    );
    let (min, max) = ie::GAIN_RANGE_DB;
    view! {
        <Fold title="Audio clip" open=true>
            <div class="field-grid">
                {source_row}
                {span_rows(page)}
                <NumberField
                    label="Level"
                    unit="dB"
                    value=Signal::derive(move || clip.get().map(|c| f64::from(c.1)))
                    step=0.5
                    decimals=1
                    min=f64::from(min)
                    max=f64::from(max)
                    send=move |db| page.apply(|s| ie::set_gain(s, &page.id(), db as f32))
                />
            </div>
        </Fold>
    }
    .into_any()
}

/// A bookmark: name, time, pausing, arrival, wait, note and keys.
fn bookmark_page(page: Page) -> AnyView {
    let mark = Memo::new(move |_| {
        page.show
            .with(|s| s.as_ref().and_then(|s| s.bookmark(&page.id()).cloned()))
    });
    let pausing = move || mark.with(|b| b.as_ref().is_some_and(|b| b.pausing));
    let toggle = move |_| {
        let to = !pausing();
        page.apply(|s| ie::set_pausing(s, &page.id(), to));
    };
    let name_row = text_row(
        "Name",
        "",
        move || mark.with(|b| b.as_ref().map(|b| b.name.clone()).unwrap_or_default()),
        move |name| page.apply(|s| edits::rename(s, &page.id(), &name)),
    );
    let at_row = clock_row(
        "At",
        "",
        move || mark.with(|b| b.as_ref().map(|b| b.at_ms)),
        move |ms| ms.is_some_and(|ms| page.apply(|s| ie::set_bookmark_at(s, &page.id(), ms))),
    );
    let arrive_row = select_row(
        "Arrive with",
        move || {
            page.read(Vec::new(), |s, c| {
                preset_options(c, &show_default_label(c, s))
            })
        },
        move || {
            mark.with(|b| {
                b.as_ref()
                    .and_then(|b| b.preset_id.as_ref().map(ToString::to_string))
                    .unwrap_or_default()
            })
        },
        move |v| {
            page.apply(|s| ie::set_arrive(s, &page.id(), preset_of(&v)));
        },
    );
    let wait_row = move || {
        clock_row(
            "Continue after",
            "Waits for Continue",
            move || mark.with(|b| b.as_ref().and_then(|b| b.continue_after_ms)),
            move |ms| page.apply(|s| ie::set_continue_after(s, &page.id(), ms)),
        )
    };
    let shown_pausing = Memo::new(move |_| pausing());
    let note_again = RwSignal::new(0_u32);
    let number = move || {
        page.show
            .with(|s| s.as_ref().and_then(|s| bookmark_number(s, &page.id())))
    };
    let go_to = move || Action::ShowGoTo {
        show_id: page.show_id.get_value().into(),
        bookmark_id: page.id(),
    };
    view! {
        <Fold title="Bookmark" open=true>
            <div class="field-grid">
                {name_row}
                {at_row}
                <div class="select-field switch-field">
                    <span class="field-label">"Pausing"</span>
                    <button type="button" role="switch" class="switch" class:on=pausing
                        aria-label="Pausing" aria-checked=move || pausing().to_string()
                        title="A pausing bookmark waits for Continue"
                        on:click=toggle>
                        <span class="switch-knob"></span>
                    </button>
                </div>
                {arrive_row}
                {move || shown_pausing.get().then(wait_row)}
            </div>
            <label class="select-field note-field">
                <span class="field-label">"Note"</span>
                <textarea rows="5" aria-label="Note" placeholder="Notes for whoever runs the show, shown in the Run view at this bookmark"
                    prop:value=move || {
                        note_again.track();
                        mark.with(|b| b.as_ref().map(|b| b.note.clone()).unwrap_or_default())
                    }
                    on:change=move |ev| {
                        let note = event_target_value(&ev);
                        if !page.apply(|s| ie::set_note(s, &page.id(), &note)) {
                            note_again.update(|n| *n += 1);
                        }
                    }></textarea>
            </label>
        </Fold>
        <Fold title="Keys" open=true>
            <HotkeyRow label="Go to" action=go_to() />
            {move || number().map(|n| {
                let label = GO_TO_LABELS[usize::from(n) - 1];
                view! {
                    <HotkeyRow label=label action=Action::ShowGoToNumber { number: n } />
                    <p class="note">
                        {format!("This is bookmark {n} in show order, so “Go to bookmark {n}” lands here.")}
                    </p>
                }
            })}
        </Fold>
    }
    .into_any()
}

/// A loop: name, ends, wrap and how it is left.
fn loop_page(page: Page) -> AnyView {
    let lp = Memo::new(move |_| {
        page.show.with(|s| {
            s.as_ref()
                .and_then(|s| s.loops.iter().find(|l| l.loop_id == page.id()).cloned())
        })
    });
    let name_row = text_row(
        "Name",
        "",
        move || lp.with(|l| l.as_ref().map(|l| l.name.clone()).unwrap_or_default()),
        move |name| page.apply(|s| edits::rename(s, &page.id(), &name)),
    );
    let start_row = clock_row(
        "Start",
        "",
        move || lp.with(|l| l.as_ref().map(|l| l.start_ms)),
        move |ms| ms.is_some_and(|ms| page.apply(|s| ie::set_loop_start(s, &page.id(), ms))),
    );
    let end_row = clock_row(
        "End",
        "",
        move || lp.with(|l| l.as_ref().map(|l| l.end_ms)),
        move |ms| ms.is_some_and(|ms| page.apply(|s| ie::set_loop_end(s, &page.id(), ms))),
    );
    let wrap_row = select_row(
        "Wrap with",
        move || {
            page.read(Vec::new(), |s, c| {
                preset_options(c, &show_default_label(c, s))
            })
        },
        move || {
            lp.with(|l| {
                l.as_ref()
                    .and_then(|l| l.preset_id.as_ref().map(ToString::to_string))
                    .unwrap_or_default()
            })
        },
        move |v| {
            page.apply(|s| ie::set_loop_preset(s, &page.id(), preset_of(&v)));
        },
    );
    let leave_row = select_row(
        "Leave",
        || {
            vec![
                ("once".to_owned(), "At once".to_owned()),
                ("finish".to_owned(), "Finish this pass".to_owned()),
            ]
        },
        move || {
            lp.with(|l| l.as_ref().map_or("once", |l| leave_key(l.leave)))
                .to_owned()
        },
        move |v| {
            let leave = if v == "finish" {
                LoopLeave::FinishPass
            } else {
                LoopLeave::AtOnce
            };
            page.apply(|s| ie::set_loop_leave(s, &page.id(), leave));
        },
    );
    view! {
        <Fold title="Loop" open=true>
            <div class="field-grid">
                {name_row}
                {start_row}
                {end_row}
                {wrap_row}
                {leave_row}
            </div>
            <p class="note">"The show repeats from Start to End until Continue, or a key for a bookmark outside it."</p>
        </Fold>
    }
    .into_any()
}

/// A keyframe marker: when, which keyframe, and with what.
fn keyframe_page(page: Page) -> AnyView {
    let marker = Memo::new(move |_| {
        page.show.with(|s| {
            s.as_ref().and_then(|s| {
                s.keyframes
                    .iter()
                    .find(|m| m.marker_id == page.id())
                    .cloned()
            })
        })
    });
    let at_row = clock_row(
        "At",
        "",
        move || marker.with(|m| m.as_ref().map(|m| m.at_ms)),
        move |ms| ms.is_some_and(|ms| page.apply(|s| ie::set_marker_at(s, &page.id(), ms))),
    );
    let keyframe_row = select_row(
        "Keyframe",
        move || {
            let m = marker.get();
            page.read(Vec::new(), |s, c| {
                let scene = m.as_ref().and_then(|m| scene_at(s, m.at_ms));
                keyframe_options(
                    c,
                    scene.as_ref(),
                    m.as_ref().and_then(|m| m.keyframe_id.as_ref()),
                )
            })
        },
        move || {
            marker.with(|m| {
                m.as_ref()
                    .and_then(|m| m.keyframe_id.as_ref().map(ToString::to_string))
                    .unwrap_or_default()
            })
        },
        move |v| {
            let preset = marker.with_untracked(|m| m.as_ref().and_then(|m| m.preset_id.clone()));
            let keyframe = (!v.is_empty()).then(|| SceneId::new(v));
            page.apply(|s| ie::set_marker(s, &page.id(), keyframe, preset));
        },
    );
    let preset_row = select_row(
        "Move with",
        move || {
            page.read(Vec::new(), |_, c| {
                preset_options(c, &collection_default_label(c))
            })
        },
        move || {
            marker.with(|m| {
                m.as_ref()
                    .and_then(|m| m.preset_id.as_ref().map(ToString::to_string))
                    .unwrap_or_default()
            })
        },
        move |v| {
            let keyframe =
                marker.with_untracked(|m| m.as_ref().and_then(|m| m.keyframe_id.clone()));
            page.apply(|s| ie::set_marker(s, &page.id(), keyframe, preset_of(&v)));
        },
    );
    view! {
        <Fold title="Keyframe" open=true>
            <div class="field-grid">
                {at_row}
                {keyframe_row}
                {preset_row}
            </div>
            <p class="note">"The scene on program moves to the keyframe's values, timed by the preset."</p>
        </Fold>
    }
    .into_any()
}

/// A cue: when, and the action it runs.
fn cue_page(page: Page) -> AnyView {
    let cue = Memo::new(move |_| {
        page.show.with(|s| {
            s.as_ref().and_then(|s| {
                s.sequences
                    .iter()
                    .flat_map(|t| &t.cues)
                    .find(|c| c.cue_id == page.id())
                    .cloned()
            })
        })
    });
    let what = move || {
        let action = cue.with(|c| c.as_ref().and_then(|c| c.action()));
        page.read(String::new(), |_, c| {
            action.map_or_else(
                || "(unknown action)".to_owned(),
                |a| crate::panels::events::action_text(c, &a),
            )
        })
    };
    view! {
        <Fold title="Cue" open=true>
            {read_row("At", move || cue.with(|c| c.as_ref().map(|c| clock_text(c.at_ms)).unwrap_or_default()))}
            {read_row("Runs", what)}
        </Fold>
    }
    .into_any()
}

/// The show's own settings, under every selection's page.
fn show_settings(page: Page) -> AnyView {
    let name_row = text_row(
        "Name",
        "",
        move || {
            page.show
                .with(|s| s.as_ref().map(|s| s.name.clone()).unwrap_or_default())
        },
        move |name| page.apply(|s| ie::set_show_name(s, &name)),
    );
    let end_row = select_row(
        "End of show",
        move || {
            page.show
                .with(|s| s.as_ref().map(end_options).unwrap_or_default())
        },
        move || {
            page.show
                .with(|s| s.as_ref().map(|s| end_key(&s.end)).unwrap_or_default())
        },
        move |v| {
            page.apply(|s| ie::set_show_end(s, end_from_key(&v)));
        },
    );
    let preset_row = select_row(
        "Transition",
        move || {
            page.read(Vec::new(), |_, c| {
                preset_options(c, &collection_default_label(c))
            })
        },
        move || {
            page.show.with(|s| {
                s.as_ref()
                    .and_then(|s| s.preset_id.as_ref().map(ToString::to_string))
                    .unwrap_or_default()
            })
        },
        move |v| {
            page.apply(|s| ie::set_show_preset(s, preset_of(&v)));
        },
    );
    let image_row = seconds_row(
        "Image length",
        3_600.0,
        move || page.show.with(|s| s.as_ref().map(|s| s.image_ms)),
        move |ms| page.apply(|s| ie::set_image_ms(s, ms)),
    );
    let planned_row = clock_row(
        "Planned length",
        "None",
        move || page.show.with(|s| s.as_ref().and_then(|s| s.planned_ms)),
        move |ms| page.apply(|s| ie::set_planned_ms(s, ms)),
    );
    let hint = Signal::derive(move || {
        page.show
            .with(|s| s.as_ref().map(|s| s.name.clone()).unwrap_or_default())
    });
    view! {
        <Fold title="Show" open=false hint=hint>
            <div class="field-grid">
                {name_row}
                {end_row}
                {preset_row}
                {image_row}
                {planned_row}
            </div>
            <p class="note">"Transition is what jumps and loops use unless they pick their own. Image length is how long a dropped image lasts."</p>
        </Fold>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::super::edits::{add_keyframe, add_loop, insert_program_clip, new_show};
    use super::*;
    use jstream_control::protocol::{SceneInfo, TransitionPresetInfo};

    fn show() -> ShowInfo {
        let mut s = new_show("friday", "Friday show");
        insert_program_clip(&mut s, "soon".into(), "Starting soon", 0, 180_000, false);
        insert_program_clip(&mut s, "main".into(), "Main", u32::MAX, 180_000, true);
        s
    }

    #[test]
    fn a_time_reads_and_types_as_m_ss() {
        assert_eq!(clock_text(90_000), "1:30");
        assert_eq!(clock_text(90_500), "1:30.5");
        assert_eq!(clock_text(3_723_000), "1:02:03");
        assert_eq!(parse_clock("1:30"), Some(90_000));
        assert_eq!(parse_clock(" 1:30.5 "), Some(90_500));
        assert_eq!(parse_clock("1:02:03"), Some(3_723_000));
        assert_eq!(parse_clock("90"), Some(90_000));
        assert_eq!(parse_clock("2,5"), Some(2_500));
        assert_eq!(parse_clock("1:75"), None);
        assert_eq!(parse_clock("1:60:00"), None);
        assert_eq!(parse_clock("soon"), None);
        assert_eq!(parse_clock(""), None);
        assert_eq!(parse_clock("-3"), None);
        for ms in [0, 1_500, 59_900, 61_000, 3_600_000] {
            assert_eq!(parse_clock(&clock_text(ms)), Some(ms));
        }
    }

    #[test]
    fn a_selection_is_found_by_its_kind() {
        let mut s = show();
        let main = s.program[1].clip_id.clone();
        let bm = s.bookmarks[0].bookmark_id.clone();
        let lp = add_loop(&mut s, 0, 60_000).expect("loop");
        let kf = add_keyframe(&mut s, 200_000, None);
        assert_eq!(kind_of(&s, &main), Some(Kind::Program));
        assert_eq!(kind_of(&s, &bm), Some(Kind::Bookmark));
        assert_eq!(kind_of(&s, &lp), Some(Kind::Loop));
        assert_eq!(kind_of(&s, &kf), Some(Kind::Keyframe));
        assert_eq!(kind_of(&s, "nope"), None);
        assert_eq!(Kind::Program.title(), "Show clip");
    }

    #[test]
    fn a_clip_lists_the_bookmarks_inside_it_from_its_start() {
        let mut s = show();
        let main = s.program[1].clip_id.clone();
        edits::add_bookmark(&mut s, 270_000).expect("added");
        if let Some(b) = s.bookmarks.iter_mut().find(|b| b.at_ms == 270_000) {
            b.name = "Close-up".into();
            b.preset_id = Some("move".into());
        }
        // One at the clip's start belongs to the clip before.
        edits::add_bookmark(&mut s, 180_000).expect("added");
        let rows = clip_bookmarks(
            &s,
            &main,
            |id| {
                s.bookmark(id)
                    .filter(|b| b.name == "Close-up")
                    .map(|_| "F2".to_owned())
            },
            |p| p.map_or_else(|| "Cut".to_owned(), |p| format!("<{p}>")),
        );
        assert_eq!(
            rows.iter()
                .map(|r| (r.label.as_str(), r.detail.as_str()))
                .collect::<Vec<_>>(),
            [
                ("Close-up · F2", "at 1:30 · arrive: <move>"),
                ("Main", "at 3:00 · arrive: Cut"),
            ]
        );
    }

    #[test]
    fn the_first_eight_bookmarks_have_numbers() {
        let mut s = show();
        for n in 1..10 {
            edits::add_bookmark(&mut s, n * 1_000).expect("added");
        }
        let nth = |n: usize| s.bookmarks[n].bookmark_id.clone();
        assert_eq!(bookmark_number(&s, &nth(0)), Some(1));
        assert_eq!(bookmark_number(&s, &nth(7)), Some(8));
        assert_eq!(bookmark_number(&s, &nth(8)), None);
    }

    #[test]
    fn the_scene_at_a_time_is_the_clip_it_falls_in() {
        let s = show();
        assert_eq!(
            scene_at(&s, 0).map(|s| s.to_string()).as_deref(),
            Some("soon")
        );
        assert_eq!(
            scene_at(&s, 180_000).map(|s| s.to_string()).as_deref(),
            Some("main")
        );
        assert_eq!(
            scene_at(&s, 999_999).map(|s| s.to_string()).as_deref(),
            Some("main")
        );
        assert_eq!(scene_at(&new_show("x", "x"), 0), None);
    }

    #[test]
    fn the_end_of_show_round_trips_through_its_picker() {
        let s = show();
        let options = end_options(&s);
        assert_eq!(options.len(), 3);
        assert_eq!(options[2].1, "Go to Main");
        for (key, _) in &options {
            assert_eq!(&end_key(&end_from_key(key)), key);
        }
    }

    fn scene(id: &str, name: &str, of: Option<&str>) -> SceneInfo {
        let mut s: SceneInfo = serde_json::from_value(serde_json::json!({
            "scene_id": id, "name": name, "items": [],
        }))
        .expect("a scene");
        s.variant_of = of.map(SceneId::from);
        s
    }

    #[test]
    fn pickers_list_scenes_keyframes_and_presets() {
        let mut c = Collection {
            scenes: vec![
                scene("main", "Main", None),
                scene("wide", "Wide", Some("main")),
                scene("outro", "Outro", None),
            ],
            ..Collection::default()
        };
        assert_eq!(
            scene_options(&c, Some(&"gone".into())),
            [
                ("gone".to_owned(), "(missing)".to_owned()),
                ("main".to_owned(), "Main".to_owned()),
                ("outro".to_owned(), "Outro".to_owned()),
            ]
        );
        let mut named = show();
        let main = named.program[1].clip_id.clone();
        let marker = add_keyframe(&mut named, 200_000, Some("wide".into()));
        assert_eq!(name_of(&named, &c, &main), "Main");
        assert_eq!(name_of(&named, &c, &marker), "Wide");
        assert_eq!(name_of(&named, &c, &named.program[0].clip_id), "(missing)");
        let kf = keyframe_options(&c, Some(&"main".into()), None);
        assert_eq!(
            kf.iter().map(|o| o.1.as_str()).collect::<Vec<_>>(),
            ["Scene's own", "Wide"]
        );
        let preset = TransitionPresetInfo {
            preset_id: "move".into(),
            name: "Move".into(),
            duration_ms: 800,
            ..jstream_control::protocol::initial_preset()
        };
        c.transitions.presets.push(preset);
        let options = preset_options(&c, "Default");
        assert_eq!(options.last().map(|o| o.1.as_str()), Some("Move 0.8 s"));
        let mut s = show();
        s.preset_id = Some("move".into());
        assert_eq!(show_default_label(&c, &s), "Default (Move 0.8 s)");
    }
}
