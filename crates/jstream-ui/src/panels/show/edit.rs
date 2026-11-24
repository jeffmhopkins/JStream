//! The Show panel's Edit view: the timeline. Its bar (the show picker and its menu, Edit /
//! Run, Stop show, Back, Continue and what it will do, Lock, the status, the
//! "+ Overlay track" and "+ Audio track" buttons and Fit show), the tool strip, the ruler
//! with the play head's flag, the tracks, the bookmark lines with their flags along the bottom, the
//! loops' bars under them, and the zoom bar over the whole show.
//!
//! Moving around: Ctrl+wheel zooms about the pointer, Shift+wheel scrolls, and the zoom
//! bar does both ([`zoom`]). A click or a drag on the ruler, or a drag of the play head,
//! moves the play head: while the show runs it seeks (`show.seek`), and while it doesn't
//! it sets where Play starts. The Run view's strip arms a spot instead, so a click there
//! never jumps program.
//!
//! Editing: a click selects a clip, marker, cue, bookmark or loop (Delete removes it,
//! Escape lets go). With Select, a drag moves or trims what it takes ([`gesture`]); the
//! other tools (Trim, Split, Bookmark, Loop, Transition, Keyframe, each with a key) do
//! what their names say. A drag is drawn as it goes and sent as one `show.set` when the
//! pointer comes up, so every gesture is one undo step. Scenes from the Scenes panel and
//! media files land on the track under the pointer ([`drops`]). A locked show refuses
//! every edit. Where things are is worked out in [`geom`], as pure functions.
//!
//! Docs: crates/jstream-ui/README.md#design

pub mod drops;
pub mod geom;
pub mod gesture;
pub mod zoom;

use super::edits;
use super::model::{ClipKind, ClipView, RunPhase, ShowView, TrackKind, TrackView};
use super::time::{Viewport, clock};
use super::{Command, ModeSwitch, ShowPanel, status};
use crate::menu::{Entry, Menus};
use geom::{HEAD_PX, Part, at_pct, span_pct};
use gesture::{Dragged, Grab, Snapper};
use jstream_control::protocol::ShowInfo;
use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;
use leptos::web_sys::Element;

/// The footer's hint.
const HINT: &str = "Drag a scene, video, image or audio file onto a track · drag a clip edge \
                    to trim · click a cut badge for its transition · Bookmark tool (B) · Loop \
                    tool (L): drag on the ruler · zoom bar: drag an end to zoom";

/// The note at the panel's top right.
const AUDIO_NOTE: &str =
    "Audio tracks are optional. With none, a show uses each scene's audio and the master, as now.";

/// How far a press must move, in px, before it is a drag rather than a click.
const DRAG_PX: i32 = 3;

/// A tool in the strip at the Edit view's left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// Pick, move and trim clips (V).
    #[default]
    Select,
    /// Drag a clip's nearer end to change its length (T).
    Trim,
    /// Cut a program clip in two where it is clicked (S).
    Split,
    /// Place a show bookmark with a click; a click on a flag opens its menu (B).
    Bookmark,
    /// Drag a loop along the ruler (L).
    Loop,
    /// Pick the transition on a cut with a click on it (X).
    Transition,
    /// Place a keyframe marker with a click on K (K).
    Keyframe,
}

impl Tool {
    /// The strip's tools, top to bottom.
    pub const ALL: [Tool; 7] = [
        Tool::Select,
        Tool::Trim,
        Tool::Split,
        Tool::Bookmark,
        Tool::Loop,
        Tool::Transition,
        Tool::Keyframe,
    ];

    /// Its symbol on the strip.
    pub fn glyph(self) -> &'static str {
        match self {
            Tool::Select => "▸",
            Tool::Trim => "⇔",
            Tool::Split => "✂",
            Tool::Bookmark => "⚑",
            Tool::Loop => "⟲",
            Tool::Transition => "⧓",
            Tool::Keyframe => "◆",
        }
    }

    /// Its name, in its tooltip, with its key.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Select (V)",
            Tool::Trim => "Trim (T)",
            Tool::Split => "Split (S)",
            Tool::Bookmark => "Bookmark (B)",
            Tool::Loop => "Loop (L)",
            Tool::Transition => "Transition (X)",
            Tool::Keyframe => "Keyframe (K)",
        }
    }

    /// The tool a key picks, ignoring case: V, T, S, B, L, X, K.
    pub fn from_key(key: &str) -> Option<Tool> {
        Some(match key.to_ascii_lowercase().as_str() {
            "v" => Tool::Select,
            "t" => Tool::Trim,
            "s" => Tool::Split,
            "b" => Tool::Bookmark,
            "l" => Tool::Loop,
            "x" => Tool::Transition,
            "k" => Tool::Keyframe,
            _ => return None,
        })
    }

    /// Its class suffix in style.css (`show-tool-<class>`).
    pub fn class(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Trim => "trim",
            Tool::Split => "split",
            Tool::Bookmark => "bookmark",
            Tool::Loop => "loop",
            Tool::Transition => "transition",
            Tool::Keyframe => "keyframe",
        }
    }
}

/// What a clip is to the gestures, by its track: its `data-kind`.
fn grab_kind(track: TrackKind, clip: &ClipView) -> &'static str {
    match (track, &clip.kind) {
        (TrackKind::Program, _) => "program",
        (TrackKind::Overlay(_), _) => "overlay",
        (TrackKind::Audio(_), _) => "audio",
        // Linked audio grabs its scene clip, which shares its ID (ADR-0216).
        (TrackKind::Linked(1), _) => "program",
        (TrackKind::Linked(_), _) => "overlay",
        _ => "marker",
    }
}

/// `left` and `width` styles for a clip from `left`% across, `width`% wide, inset 1 px
/// each side so neighbours show a gap.
fn placed(left: f64, width: f64) -> (String, String) {
    (
        format!("calc({left:.4}% + 1px)"),
        format!("calc({width:.4}% - 2px)"),
    )
}

/// The small clock on a timed clip: it holds a video or audio file, and a jump into it
/// seeks.
fn timed_badge() -> impl IntoView {
    view! {
        <i class="show-timed" title="Timed: it plays a file, and a jump into it seeks">
            <svg viewBox="0 0 12 12" width="11" height="11" aria-hidden="true">
                <circle cx="6" cy="6" r="4.8" fill="none" stroke="currentColor" stroke-width="1.3" />
                <path d="M6 3.2 V6 L8 7.2" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
            </svg>
        </i>
    }
}

/// A clip's name, with "(missing)" when what it names is gone.
fn name(clip: &ClipView) -> String {
    if clip.missing {
        format!("{} (missing)", clip.label)
    } else {
        clip.label.clone()
    }
}

/// The fade handles on a selected overlay or audio clip: at the end of its fade in and
/// the start of its fade out.
fn fade_handles(clip: &ClipView) -> Option<impl IntoView + use<>> {
    let (fade_in, fade_out) = clip.fades?;
    let len = f64::from(clip.len_ms.max(1));
    let a = f64::from(fade_in) / len * 100.0;
    let b = (100.0 - f64::from(fade_out) / len * 100.0).max(a);
    Some(view! {
        <i class="show-fade-h" style:left=format!("{a:.3}%") title="Drag to set the fade in"></i>
        <i class="show-fade-h" style:left=format!("{b:.3}%") title="Drag to set the fade out"></i>
    })
}

/// One clip of a track in the part of the show `view` shows, or nothing when it is out of
/// view. `n` is its place on its track, for the program colours.
fn clip_view(
    kind: TrackKind,
    n: usize,
    clip: &ClipView,
    view: Viewport,
    selected: bool,
) -> Option<AnyView> {
    let marker = clip.len_ms == 0;
    let id = clip.id.clone();
    let grab = grab_kind(kind, clip);
    if marker {
        let left = format!("{:.4}%", at_pct(view, clip.start_ms)?);
        return Some(match clip.kind {
            ClipKind::Keyframe(_) => view! {
                <div class="show-kf" class:missing=clip.missing class:show-sel=selected
                    data-clip=id data-kind=grab style:left=left title=name(clip)>
                    <s></s>{name(clip)}
                </div>
            }
            .into_any(),
            _ => view! {
                <div class="show-ev" class:missing=clip.missing class:show-sel=selected
                    data-clip=id data-kind=grab style:left=left title=name(clip)>
                    <span>{name(clip)}</span>
                </div>
            }
            .into_any(),
        });
    }
    let (left, width) = span_pct(view, clip.start_ms, clip.end_ms())?;
    let (left, width) = placed(left, width);
    let title = if clip.detail.is_empty() {
        name(clip)
    } else {
        format!("{} · {}", name(clip), clip.detail)
    };
    let badge = clip.timed.then(timed_badge);
    let handles = selected.then(|| fade_handles(clip)).flatten();
    Some(match (kind, &clip.kind) {
        (TrackKind::Program, _) => view! {
            <div class=format!("show-clip show-prog show-clip-c{}", n % geom::PROGRAM_COLOURS)
                class:missing=clip.missing class:show-sel=selected
                data-clip=id data-kind=grab style:left=left style:width=width title=title>
                <b>{name(clip)}{badge}</b>
                <span>{clip.detail.clone()}</span>
                {selected.then(|| view! { <i class="show-grab-r"></i> })}
            </div>
        }
        .into_any(),
        (_, ClipKind::Sequence(_)) => view! {
            <div class="show-seq" class:missing=clip.missing class:show-sel=selected
                data-clip=id data-kind=grab style:left=left style:width=width title=title>
                <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
                    <path d=geom::curve_path(&clip.curve) vector-effect="non-scaling-stroke" />
                </svg>
                <b>"▶ "{name(clip)}{badge}</b>
            </div>
        }
        .into_any(),
        (_, ClipKind::Linked(_)) => view! {
            <div class="show-au show-linked" class:missing=clip.missing class:show-sel=selected
                data-clip=id data-kind=grab style:left=left style:width=width
                title=format!("{title} · linked to its scene clip")>
                <b><i class="show-link" aria-hidden="true">"🔗"</i>{name(clip)}</b>
                <span>{clip.detail.clone()}</span>
            </div>
        }
        .into_any(),
        (_, ClipKind::Audio(_)) => {
            let fades = clip.fades.unwrap_or((0, 0));
            view! {
                <div class="show-au" class:missing=clip.missing class:show-sel=selected
                    data-clip=id data-kind=grab style:left=left style:width=width title=title>
                    <svg viewBox="0 0 1000 100" preserveAspectRatio="none" aria-hidden="true">
                        <path class="fill" d=geom::fade_path(clip.len_ms, fades, true) />
                        <path class="line" d=geom::fade_path(clip.len_ms, fades, false) vector-effect="non-scaling-stroke" />
                    </svg>
                    <b>{name(clip)}{badge}</b>
                    <span>{clip.detail.clone()}</span>
                    {handles}
                </div>
            }
            .into_any()
        }
        _ => view! {
            <div class="show-clip show-ov" class:missing=clip.missing class:show-sel=selected
                data-clip=id data-kind=grab style:left=left style:width=width title=title>
                <b>{name(clip)}{badge}</b>
                <span>{clip.detail.clone()}</span>
                {handles}
            </div>
        }
        .into_any(),
    })
}

/// The transition badges on program's cuts in view: "⧓ Fade 0.5 s" over each cut. A
/// click opens the cut's transition menu.
fn cuts(track: &TrackView, view: Viewport) -> Vec<AnyView> {
    track
        .clips
        .iter()
        .filter_map(|c| {
            let label = c.transition_in.clone()?;
            let left = at_pct(view, c.start_ms)?;
            Some(
                view! {
                    <div class="show-cut" style:left=format!("{left:.4}%") data-cut=c.id.clone()
                        title=format!("Transition into {}: {label} · click to change", c.label)>
                        <i>"⧓"</i>{label.clone()}
                    </div>
                }
                .into_any(),
            )
        })
        .collect()
}

/// One track's row: its head and its clips. `slot` draws the "Drop a scene or a file"
/// target `slot`% across V1, while something is dragged over the tracks; `rename` is
/// the name field over the head while the track is being renamed.
fn track_row(
    track: &TrackView,
    previous: Option<TrackKind>,
    view: Viewport,
    selected: Option<&str>,
    (over, slot): (bool, Option<f64>),
    (lifted, target): (bool, bool),
    rename: Option<AnyView>,
) -> AnyView {
    let kind = track.kind;
    let mut body: Vec<AnyView> = track
        .clips
        .iter()
        .enumerate()
        .filter_map(|(n, c)| clip_view(kind, n, c, view, selected == Some(c.id.as_str())))
        .collect();
    if kind == TrackKind::Program {
        body.extend(cuts(track, view));
        if let Some(left) = slot {
            body.push(
                view! {
                    <div class="show-slot" style:left=format!("calc({left:.4}% + 4px)")>
                        "Drop a scene or a file"
                    </div>
                }
                .into_any(),
            );
        }
    }
    let muted = track.muted;
    let locked = track.locked;
    let title = if track.name.is_empty() {
        kind.title().to_owned()
    } else {
        track.name.clone()
    };
    view! {
        <div class=format!("show-track show-track-{}", kind.class())
            class:show-group=geom::starts_group(previous, kind)
            class:muted=muted
            class:drop-over=over
            class:lifted=lifted
            class:reorder-target=target
            style:height=format!("{}px", geom::row_px(kind))>
            <div class="show-track-head" data-track-head=track.id.clone() data-kind=kind.class()
                title="Drag to reorder · right-click for the track's menu">
                <b><span class="show-tag">{kind.tag()}</span>{title}</b>
                {rename}
                <span class="show-track-icons">
                    <i class="show-mute" class:on=muted title=if muted { "Muted" } else { "Plays" }>"◉"</i>
                    <i class="show-lock" class:on=locked title=if locked { "Locked" } else { "Unlocked" }>"🔒"</i>
                </span>
            </div>
            <div class="show-track-body" data-track=track.id.clone() data-kind=kind.class()>{body}</div>
        </div>
    }
    .into_any()
}

/// What a press will do if it comes up without moving.
#[derive(Clone, Debug, PartialEq)]
enum Click {
    /// Nothing more than the press did.
    Nothing,
    /// Split program clip (ID) at a time.
    Split(String, f64),
    /// Add a bookmark at a time.
    Bookmark(f64),
    /// Open bookmark (ID)'s menu.
    BookmarkMenu(String),
    /// Open the transition menu of the cut into program clip (ID).
    Cut(String),
    /// Open the keyframe menu for a marker at a time.
    Keyframe(f64),
}

/// A press on the timeline, from the pointer down to its release.
#[derive(Clone, Debug)]
struct Press {
    /// What a drag moves, if the press took something.
    grab: Option<Grab>,
    /// The show when the press began.
    base: Option<ShowInfo>,
    /// Where the press was, in show time.
    from_ms: f64,
    /// Where it was on screen.
    at: (i32, i32),
    /// Whether it moved far enough to be a drag.
    moved: bool,
    /// What it does if it never moves.
    click: Click,
    /// The drag's outcome so far.
    last: Option<Dragged>,
}

/// The tip that follows the pointer during a drag.
#[derive(Clone, Debug, PartialEq)]
struct Tip {
    text: String,
    x: i32,
    y: i32,
    refused: bool,
}

/// Something dragged over the tracks: the track under the pointer, the time there, and
/// whether it is over the drop slot at the end of V1.
#[derive(Clone, Debug, PartialEq)]
struct Hover {
    track: String,
    at_ms: f64,
    slot: bool,
}

/// The element an event happened on.
fn target_of(e: &leptos::web_sys::Event) -> Option<Element> {
    e.target().and_then(|t| t.dyn_into::<Element>().ok())
}

/// The nearest element around `el` matching `selector`.
fn closest(el: &Element, selector: &str) -> Option<Element> {
    el.closest(selector).ok().flatten()
}

/// The Edit view.
#[component]
pub fn EditView() -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let menus = use_context::<Menus>();
    let open_project = use_context::<crate::project::OpenProject>();
    let viewport = panel.viewport;
    let tool = RwSignal::new(Tool::Select);
    let show = Memo::new(move |_| panel.show());
    let running = Memo::new(move |_| panel.is_running());
    // The rows' width in px (the view less the track heads), measured with the clock's
    // tick so a dock resize follows.
    let width = RwSignal::new(0.0_f64);
    let scroller = NodeRef::<leptos::html::Div>::new();
    let root = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        panel.clock.track();
        if let Some(el) = scroller.get() {
            let w = (f64::from(el.client_width()) - HEAD_PX).max(0.0);
            if (w - width.get_untracked()).abs() > 0.5 {
                width.set(w);
            }
        }
    });
    // The view follows the show's length, with room after its end to drop the next
    // clip: the whole show while it was all in view, else kept as it is.
    Effect::new(move |_| {
        let len = geom::padded_len(show.with(|s| s.as_ref().map_or(0, ShowView::length_ms)));
        let now = viewport.get_untracked();
        let next = if now.span_ms >= now.len_ms - 1.0 {
            Viewport::fit(len)
        } else {
            now.with_len(len)
        };
        if next != now {
            viewport.set(next);
        }
    });
    // A drag's picture lasts until the engine's answer brings the show it sent.
    Effect::new(move |_| {
        panel.shows.track();
        panel.preview.set(None);
    });

    let send = move |c: Command| panel.send(c);
    let show_id = move || show.with(|s| s.as_ref().map(|s| s.id.clone()).unwrap_or_default());
    let phase = move || panel.run.with(|r| r.phase.clone());
    let paused = move || matches!(phase(), RunPhase::Paused { .. });
    let locked = move || show.with(|s| s.as_ref().is_some_and(|s| s.locked));
    let locked_now = move || show.with_untracked(|s| s.as_ref().is_some_and(|s| s.locked));
    let continue_ = move |_| send(Command::Continue);
    let next = move || {
        let head = panel.run.with(|r| r.at_ms);
        let at_too = !running.get() || paused();
        let from = if running.get() { head } else { 0 };
        let names = show.with(|s| {
            s.as_ref()
                .map(|s| geom::next_up(s, from, at_too))
                .unwrap_or_default()
        });
        (!names.is_empty()).then(|| {
            let last = names.len() - 1;
            let names = names
                .into_iter()
                .enumerate()
                .map(|(n, name)| view! { <b>{name}</b>{(n < last).then_some(", ")} })
                .collect_view();
            view! { <span class="show-next">"Next: "{names}</span> }
        })
    };
    let state = move || {
        let now = panel.clock.get();
        let text = if running.get() {
            panel
                .run
                .with(|r| show.with(|s| s.as_ref().map(|s| status(s, r, now))))
        } else {
            // Another show may be running; this one isn't.
            let other = panel.run.with(|r| r.show.clone()).and_then(|id| {
                panel
                    .shows
                    .with(|shows| shows.iter().find(|s| s.id == id).map(|s| s.name.clone()))
            });
            Some(match other {
                Some(name) => format!("Stopped · {name} is running"),
                None => "Stopped".to_owned(),
            })
        };
        let glyph = (running.get() && paused()).then_some("❚❚ ");
        view! {
            <span class="show-state" class:idle=move || !running.get()>{glyph}{text}</span>
        }
    };
    let add_audio_track = move || {
        let mut added = true;
        panel.set_show(&show_id(), |s| added = edits::add_audio_track(s).is_some());
        if !added {
            panel.refuse("A show holds at most 8 audio tracks.");
        }
    };

    let bar = view! {
        <div class="show-bar show-edit-bar">
            <ModeSwitch />
            <button type="button" class="show-btn" disabled=move || !running.get()
                on:click=move |_| send(Command::Stop)>"■ Stop show"</button>
            <button type="button" class="show-btn" disabled=move || !running.get()
                title="Back to the previous pausing bookmark"
                on:click=move |_| send(Command::Back)>"⏮ Back"</button>
            <button type="button" class="show-btn show-play"
                class:on=move || running.get() && !paused()
                title=move || if running.get() && !paused() {
                    "Pause where the play head is"
                } else {
                    "Play from the play head"
                }
                on:click=move |_| send(if running.get_untracked() && !paused() {
                    Command::Pause
                } else {
                    Command::Play(show_id())
                })>
                {move || if running.get() && !paused() { "❚❚ Pause" } else { "▶ Play" }}
            </button>
            <button type="button" class="primary show-continue"
                disabled=move || !running.get() || matches!(phase(), RunPhase::Transition)
                title="Play on from a pause, or on to the next pausing bookmark"
                on:click=continue_>"Continue ▸"</button>
            {move || panel.continue_key.get().map(|k| view! { <kbd class="show-key">{k}</kbd> })}
            {next}
            <span class="show-bar-gap"></span>
            <button type="button" class="show-btn show-lock" class:on=locked
                aria-pressed=move || locked().to_string()
                title=move || if locked() { "Editing is locked: unlock it" } else { "Lock the show against edits" }
                on:click=move |_| send(Command::Lock(show_id(), !locked()))>
                {move || if locked() { "🔒 Locked" } else { "🔓 Lock" }}
            </button>
            {state}
            <button type="button" class="show-btn" disabled=locked
                on:click=move |_| send(Command::AddOverlayTrack(show_id()))>"+ Overlay track"</button>
            <button type="button" class="show-btn" disabled=locked
                on:click=move |_| add_audio_track()>"+ Audio track"</button>
            <button type="button" class="show-btn"
                on:click=move |_| {
                    let len = show.with(|s| s.as_ref().map_or(0, ShowView::length_ms));
                    viewport.set(Viewport::fit(geom::padded_len(len)));
                }>"Fit show"</button>
        </div>
    };

    let tools = view! {
        <div class="show-tools" role="toolbar" aria-label="Tools" aria-orientation="vertical">
            {Tool::ALL.into_iter().map(|t| view! {
                <button type="button" class=format!("show-tool show-tool-{}", t.class())
                    class:on=move || tool.get() == t
                    aria-pressed=move || (tool.get() == t).to_string()
                    disabled=locked
                    title=t.name() aria-label=t.name()
                    on:click=move |_| tool.set(t)>{t.glyph()}</button>
            }).collect_view()}
        </div>
    };

    // Time and snapping.
    let ms_at = move |client_x: f64| {
        let left = scroller
            .get_untracked()
            .map_or(0.0, |el| el.get_bounding_client_rect().left());
        viewport
            .get_untracked()
            .ms(client_x - left - HEAD_PX, width.get_untracked())
    };
    let snapper = move |base: &ShowInfo, free: bool| {
        let mut points = edits::snap_points(base);
        if running.get_untracked() {
            points.push(
                panel
                    .run
                    .with_untracked(|r| r.head_at(panel.clock.get_untracked())),
            );
            points.sort_unstable();
        }
        let v = viewport.get_untracked();
        Snapper {
            points,
            reach: edits::snap_reach(v.span_ms / width.get_untracked().max(1.0)),
            free,
        }
    };
    let base_now = move || panel.info(&show_id());
    let snap_now =
        move |ms: f64| base_now().map_or(ms.max(0.0) as u32, |b| snapper(&b, false).at(ms));

    // Selection and the timeline's own menus.
    let selected = panel.selected;
    let press = StoredValue::new(None::<Press>);
    let pending = StoredValue::new(None::<Click>);
    // A scrub of the play head along the ruler, while it lasts: when it last sent a seek.
    let scrub = StoredValue::new(None::<f64>);
    // A track head being dragged to reorder the rows: its ID, and the head it is over.
    let lifting = RwSignal::new(None::<(String, Option<String>)>);
    let scrub_to = move |ms: f64, shift: bool, last: bool| {
        let at = if shift {
            ms.max(0.0) as u32
        } else {
            snap_now(ms)
        };
        let now = crate::store::now_ms();
        let running = untrack(|| panel.is_running());
        // While a show runs each seek jumps program, so a drag sends at most ~12 a second.
        let due = scrub.get_value().is_none_or(|sent| now - sent >= 80.0);
        if !running || due || last {
            panel.move_head(at);
            scrub.set_value(Some(now));
        }
    };
    let tip = RwSignal::new(None::<Tip>);
    let renaming = RwSignal::new(None::<String>);
    let bookmark_menu = move |e: &leptos::ev::MouseEvent, id: String| {
        let Some(menus) = menus else { return };
        let Some(pausing) = base_now().and_then(|b| {
            b.bookmarks
                .iter()
                .find(|m| m.bookmark_id == id)
                .map(|m| m.pausing)
        }) else {
            return;
        };
        let off = locked_now();
        let toggle_id = id.clone();
        let toggle = move |want: bool| {
            let id = toggle_id.clone();
            move || {
                if want != pausing {
                    panel.set_show(&show_id(), |s| {
                        edits::toggle_pausing(s, &id);
                    });
                }
            }
        };
        let (rename_id, delete_id) = (id.clone(), id.clone());
        menus.open(
            e,
            vec![
                Entry::item("Pausing", toggle(true))
                    .checked(pausing)
                    .disabled_if(off),
                Entry::item("Plays through", toggle(false))
                    .checked(!pausing)
                    .disabled_if(off),
                Entry::Sep,
                Entry::item("Rename", move || renaming.set(Some(rename_id.clone())))
                    .disabled_if(off),
                Entry::item("Delete", move || {
                    panel.set_show(&show_id(), |s| {
                        edits::remove(s, &delete_id);
                    });
                })
                .danger()
                .disabled_if(off),
            ],
        );
    };
    let cut_menu = move |e: &leptos::ev::MouseEvent, clip: String| {
        let Some(menus) = menus else { return };
        let Some(current) = base_now().and_then(|b| {
            b.program
                .iter()
                .find(|c| c.clip_id == clip)
                .map(|c| c.preset_id.clone())
        }) else {
            return;
        };
        let presets = panel
            .collection(|c| {
                c.transitions
                    .presets
                    .iter()
                    .map(|p| {
                        (
                            p.preset_id.clone(),
                            super::wire::preset_badge(c, Some(&p.preset_id)),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let off = locked_now();
        let pick = move |preset: Option<jstream_control::protocol::TransitionPresetId>| {
            let clip = clip.clone();
            move || {
                let preset = preset.clone();
                panel.set_show(&show_id(), |s| {
                    edits::set_cut(s, &clip, preset);
                });
            }
        };
        let mut entries = vec![
            Entry::Head("Transition in".to_owned()),
            Entry::item("Default", pick(None))
                .checked(current.is_none())
                .disabled_if(off),
        ];
        entries.extend(presets.into_iter().map(|(id, label)| {
            let on = current.as_ref() == Some(&id);
            Entry::item(label, pick(Some(id)))
                .checked(on)
                .disabled_if(off)
        }));
        menus.open(e, entries);
    };
    let keyframe_menu = move |e: &leptos::ev::MouseEvent, at: u32| {
        let Some(menus) = menus else { return };
        let scene = base_now().and_then(|b| {
            b.program
                .iter()
                .zip(b.program_starts())
                .take_while(|(_, start)| *start <= at)
                .last()
                .map(|(c, _)| c.scene_id.clone())
        });
        let Some(scene) = scene else {
            panel.refuse(
                "A keyframe loads into the scene on program: put a scene on V1 there first.",
            );
            return;
        };
        let keyframes = panel
            .collection(|c| {
                c.keyframes_of(&scene)
                    .map(|k| (k.scene_id.clone(), k.name.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let add = move |keyframe: Option<jstream_control::protocol::SceneId>| {
            move || {
                let keyframe = keyframe.clone();
                panel.set_show(&show_id(), |s| {
                    edits::add_keyframe(s, at, keyframe);
                });
            }
        };
        let mut entries = vec![
            Entry::Head(format!("Keyframe at {}", clock(at))),
            Entry::item("Scene's own", add(None)),
        ];
        entries.extend(
            keyframes
                .into_iter()
                .map(|(id, name)| Entry::item(name, add(Some(id)))),
        );
        menus.open(e, entries);
    };
    // A track's menu: on its head (`head`) the track's own entries too, and clips go in at
    // the play head; on its body they go in where it was clicked.
    let track_menu = move |e: &leptos::ev::MouseEvent,
                           track: String,
                           kind: String,
                           at: u32,
                           head: bool| {
        let Some(menus) = menus else { return };
        let off = locked_now();
        let mut entries = Vec::new();
        let at = if head { untrack(|| panel.head()) } else { at };
        let view = show.with_untracked(|s| {
            s.as_ref()
                .and_then(|s| s.tracks.iter().find(|t| t.id == track).cloned())
        });
        let own = matches!(
            view.as_ref().map(|t| t.kind),
            Some(TrackKind::Overlay(_) | TrackKind::Audio(_))
        );
        if let Some(t) = view
            .clone()
            .filter(|t| matches!(t.kind, TrackKind::Program | TrackKind::Overlay(_)))
        {
            let scenes = panel
                .collection(|c| super::inspector::scene_options(c, None))
                .unwrap_or_default();
            let items: Vec<Entry> = scenes
                .into_iter()
                .map(|(scene, name)| {
                    let t = t.clone();
                    Entry::item(name.clone(), move || {
                        drops::place_scene(&panel, &show_id(), &t, (&scene, &name), at, false);
                    })
                    .disabled_if(off)
                })
                .collect();
            entries.push(if items.is_empty() {
                Entry::unavailable("Add scene (no scenes yet)")
            } else {
                Entry::submenu(format!("Add scene at {}", clock(at)), items)
            });
            entries.push(Entry::Sep);
        }
        // Any source's audio, from anywhere in the project: on this A track, or on a new
        // one from any other row (ADR-0216's linked audio is the scene's own).
        {
            let on = (kind == TrackKind::Audio(1).class()).then(|| track.clone());
            let sources = panel
                .collection(|c| {
                    c.sources
                        .iter()
                        .filter(|s| s.audio.is_some())
                        .map(|s| (s.source_id.clone(), s.name.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let items = |whole: bool| -> Vec<Entry> {
                sources
                        .iter()
                        .cloned()
                        .map(|(source, name)| {
                            let on = on.clone();
                            Entry::item(name.clone(), move || {
                                let mut fits = true;
                                panel.set_show(&show_id(), |s| {
                                    fits = edits::place_source_audio(
                                        s,
                                        on.as_deref(),
                                        source.clone(),
                                        at,
                                        drops::DEFAULT_SCENE_MS,
                                        whole,
                                    )
                                    .is_some();
                                });
                                if !fits {
                                    panel.refuse(format!(
                                        "{name} doesn't fit on this track there: try another A track, or a row that isn't one for a new track."
                                    ));
                                }
                            })
                            .disabled_if(off)
                        })
                        .collect()
            };
            let new = if on.is_some() { "" } else { " (new A track)" };
            if sources.is_empty() {
                entries.push(Entry::unavailable("Add source audio (no audio sources)"));
            } else {
                entries.push(Entry::submenu(
                    format!("Add source audio at {}{new}", clock(at)),
                    items(false),
                ));
                entries.push(Entry::submenu(
                    format!("Add source audio for the whole show{new}"),
                    items(true),
                ));
            }
            entries.push(Entry::Sep);
        }
        if head && own {
            let rename_id = track.clone();
            entries.push(
                Entry::item("Rename track", move || {
                    renaming.set(Some(rename_id.clone()))
                })
                .disabled_if(off),
            );
            entries.push(Entry::Sep);
        }
        // New tracks come from an empty spot's menu, not a track's own (UI-DESIGN ›
        // Menus); the bar has them too.
        if !head {
            entries.push(
                Entry::item("+ Overlay track", move || {
                    send(Command::AddOverlayTrack(show_id()));
                })
                .disabled_if(off),
            );
            entries.push(Entry::item("+ Audio track", add_audio_track).disabled_if(off));
        }
        // Removal is the last group (UI-DESIGN › Menus).
        if head && own {
            let delete_id = track.clone();
            entries.push(
                Entry::item("Delete track", move || {
                    panel.set_show(&show_id(), |s| {
                        edits::delete_track(s, &delete_id);
                    });
                })
                .danger()
                .disabled_if(off),
            );
        }
        while matches!(entries.last(), Some(Entry::Sep)) {
            entries.pop();
        }
        menus.open(e, entries);
    };
    let remove_selected = move || {
        let Some(id) = selected.get_untracked() else {
            return;
        };
        if locked_now() {
            panel.refuse("The show is locked: unlock it to edit.");
            return;
        }
        panel.set_show(&show_id(), |s| {
            edits::remove(s, &id);
        });
        selected.set(None);
    };

    // A press: select what it lands on and work out what a drag or a click will do.
    let down = move |e: leptos::ev::PointerEvent| {
        if e.button() != 0 {
            return;
        }
        let Some(target) = target_of(&e) else {
            return;
        };
        if closest(&target, "input, button").is_some() {
            return;
        }
        if let Some(r) = root.get_untracked() {
            let _ = r.focus();
        }
        if let Some(head) = closest(&target, "[data-track-head]") {
            e.prevent_default();
            if let Some(el) = e
                .current_target()
                .and_then(|t| t.dyn_into::<Element>().ok())
            {
                let _ = el.set_pointer_capture(e.pointer_id());
            }
            let id = head.get_attribute("data-track-head").unwrap_or_default();
            lifting.set(Some((id, None)));
            return;
        }
        let ms = ms_at(f64::from(e.client_x()));
        let tool = tool.get_untracked();
        // The play head, or the ruler with a tool that doesn't draw on it, scrubs.
        let on_ruler = closest(&target, ".show-ruler").is_some()
            && !matches!(tool, Tool::Loop | Tool::Bookmark);
        if on_ruler || closest(&target, ".show-head").is_some() {
            e.prevent_default();
            if let Some(el) = e
                .current_target()
                .and_then(|t| t.dyn_into::<Element>().ok())
            {
                let _ = el.set_pointer_capture(e.pointer_id());
            }
            scrub.set_value(None);
            scrub_to(ms, e.shift_key(), true);
            return;
        }
        let base = base_now();
        let view = show.get_untracked();
        let clip_of = |id: &str| {
            view.as_ref().and_then(|v| {
                v.tracks
                    .iter()
                    .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
                    .find(|(_, c)| c.id == id)
                    .map(|(t, c)| (t.clone(), c.clone()))
            })
        };
        let mut grab = None;
        let mut click = Click::Nothing;
        let rect_x = |el: &Element| {
            let r = el.get_bounding_client_rect();
            (
                f64::from(e.client_x()) - r.left(),
                f64::from(e.client_y()) - r.top(),
                r.width(),
            )
        };
        let attr = |el: &Element, name: &str| el.get_attribute(name).unwrap_or_default();
        if let Some(el) = closest(&target, "[data-cut]") {
            click = Click::Cut(attr(&el, "data-cut"));
        } else if let Some(el) = closest(&target, "[data-bm]") {
            let id = attr(&el, "data-bm");
            selected.set(Some(id.clone()));
            if tool == Tool::Bookmark {
                click = Click::BookmarkMenu(id);
            } else {
                grab = Some(Grab::Bookmark(id));
            }
        } else if let Some(el) = closest(&target, "[data-loop]") {
            let id = attr(&el, "data-loop");
            selected.set(Some(id.clone()));
            let (x, _, w) = rect_x(&el);
            grab = Some(Grab::Loop(id, geom::loop_part(x, w)));
        } else if closest(&target, ".show-ruler").is_some() {
            if tool == Tool::Loop {
                grab = Some(Grab::NewLoop);
            } else if tool == Tool::Bookmark {
                click = Click::Bookmark(ms);
            }
        } else if let Some(el) = closest(&target, "[data-clip]") {
            let id = attr(&el, "data-clip");
            let kind = attr(&el, "data-kind");
            selected.set(Some(id.clone()));
            let (x, y, w) = rect_x(&el);
            let on_k = closest(&el, "[data-kind=keyframes]").is_some();
            grab = match (tool, kind.as_str()) {
                (Tool::Select, "program") => Some(match geom::clip_part(x, y, w, None, true) {
                    Part::End => Grab::ProgramEnd(id.clone()),
                    _ => Grab::Program(id.clone()),
                }),
                (Tool::Select, "overlay" | "audio") => {
                    let fades = clip_of(&id)
                        .and_then(|(_, c)| c.fades.map(|f| geom::fade_handles(c.len_ms, f, w)));
                    Some(Grab::Span(
                        id.clone(),
                        geom::clip_part(x, y, w, fades, false),
                    ))
                }
                (Tool::Select | Tool::Trim, "marker") => Some(Grab::Marker(id.clone())),
                (Tool::Trim, "program") => {
                    let program = view
                        .as_ref()
                        .map(|v| v.program().to_vec())
                        .unwrap_or_default();
                    let n = program.iter().position(|c| c.id == id).unwrap_or(0);
                    let edge = if geom::nearer_end(x, w) || n == 0 {
                        id.clone()
                    } else {
                        program[n - 1].id.clone()
                    };
                    Some(Grab::ProgramEnd(edge))
                }
                (Tool::Trim, "overlay" | "audio") => Some(Grab::Span(
                    id.clone(),
                    if geom::nearer_end(x, w) {
                        Part::End
                    } else {
                        Part::Start
                    },
                )),
                _ => None,
            };
            click = match (tool, kind.as_str()) {
                (Tool::Split, "program") => Click::Split(id, ms),
                (Tool::Bookmark, _) => Click::Bookmark(ms),
                (Tool::Keyframe, _) if on_k => Click::Keyframe(ms),
                (Tool::Transition, "program") => {
                    if clip_of(&id).is_some_and(|(_, c)| c.transition_in.is_some()) {
                        Click::Cut(id)
                    } else {
                        Click::Nothing
                    }
                }
                _ => Click::Nothing,
            };
        } else if let Some(el) = closest(&target, "[data-track]") {
            let kind = attr(&el, "data-kind");
            click = match tool {
                Tool::Bookmark => Click::Bookmark(ms),
                Tool::Keyframe if kind == TrackKind::Keyframes.class() => Click::Keyframe(ms),
                _ => {
                    selected.set(None);
                    Click::Nothing
                }
            };
        }
        if locked_now() {
            grab = None;
            if !matches!(click, Click::Cut(_) | Click::BookmarkMenu(_)) {
                click = Click::Nothing;
            }
        }
        if grab.is_some() {
            e.prevent_default();
            if let Some(el) = e
                .current_target()
                .and_then(|t| t.dyn_into::<Element>().ok())
            {
                let _ = el.set_pointer_capture(e.pointer_id());
            }
        }
        press.set_value(Some(Press {
            grab,
            base,
            from_ms: ms,
            at: (e.client_x(), e.client_y()),
            moved: false,
            click,
            last: None,
        }));
    };
    let moving = move |e: leptos::ev::PointerEvent| {
        if let Some((id, over)) = lifting.get_untracked() {
            let now = crate::host::element_at(f64::from(e.client_x()), f64::from(e.client_y()))
                .and_then(|el| closest(&el, "[data-track-head]"))
                .and_then(|el| el.get_attribute("data-track-head"))
                .filter(|o| *o != id);
            if now != over {
                lifting.set(Some((id, now)));
            }
            return;
        }
        if scrub.get_value().is_some() {
            scrub_to(ms_at(f64::from(e.client_x())), e.shift_key(), false);
            return;
        }
        let Some(mut p) = press.get_value() else {
            return;
        };
        let (Some(grab), Some(base)) = (p.grab.clone(), p.base.clone()) else {
            return;
        };
        if !p.moved {
            let (dx, dy) = (e.client_x() - p.at.0, e.client_y() - p.at.1);
            if dx * dx + dy * dy < DRAG_PX * DRAG_PX {
                return;
            }
            p.moved = true;
        }
        let at = ms_at(f64::from(e.client_x()));
        let d = gesture::dragged(&base, &grab, p.from_ms, at, &snapper(&base, e.shift_key()));
        panel.preview.set(panel.view_of(&d.show));
        tip.set(Some(Tip {
            text: d.refused.clone().unwrap_or_else(|| d.tip.clone()),
            x: e.client_x(),
            y: e.client_y(),
            refused: d.refused.is_some(),
        }));
        p.last = Some(d);
        press.set_value(Some(p));
    };
    let up = move |e: leptos::ev::PointerEvent| {
        if let Some((id, over)) = lifting.get_untracked() {
            lifting.set(None);
            let Some(onto) = over else { return };
            if locked_now() {
                panel.refuse("The show is locked: unlock it to edit.");
                return;
            }
            let kinds = show.with_untracked(|s| {
                let kind = |t: &str| {
                    s.as_ref()
                        .and_then(|s| s.tracks.iter().find(|x| x.id == t))
                        .map(|x| x.kind)
                };
                kind(&id).zip(kind(&onto))
            });
            let Some((from, to)) = kinds else { return };
            if let Some(why) = geom::reorder_refusal(from, to) {
                panel.refuse(why);
                return;
            }
            panel.set_show(&show_id(), |s| {
                edits::move_track(s, &id, &onto);
            });
            return;
        }
        if scrub.get_value().is_some() {
            scrub_to(ms_at(f64::from(e.client_x())), e.shift_key(), true);
            scrub.set_value(None);
            return;
        }
        let Some(p) = press.get_value() else {
            return;
        };
        press.set_value(None);
        tip.set(None);
        if !p.moved {
            pending.set_value(Some(p.click));
            return;
        }
        pending.set_value(None);
        match p.last {
            Some(Dragged {
                refused: Some(why), ..
            }) => {
                panel.preview.set(None);
                if p.grab == Some(Grab::NewLoop) {
                    panel.refuse(why);
                }
            }
            Some(d) => {
                let unchanged = p.base.as_ref() == Some(&d.show);
                panel.commit(d.show);
                if unchanged {
                    panel.preview.set(None);
                } else {
                    // Should the engine never answer, the picture still goes.
                    set_timeout(
                        move || panel.preview.set(None),
                        std::time::Duration::from_millis(1500),
                    );
                }
            }
            None => panel.preview.set(None),
        }
    };
    let cancel = move || {
        lifting.set(None);
        scrub.set_value(None);
        press.set_value(None);
        tip.set(None);
        panel.preview.set(None);
    };
    let clicked = move |e: leptos::ev::MouseEvent| {
        let Some(click) = pending.get_value() else {
            return;
        };
        pending.set_value(None);
        match click {
            Click::Nothing => {}
            Click::Split(id, ms) => {
                let at = snap_now(ms);
                let mut split = true;
                panel.set_show(&show_id(), |s| {
                    split = edits::split_program_clip(s, &id, at).is_some();
                });
                if !split {
                    panel.refuse("Split a clip inside it, away from its ends.");
                }
            }
            Click::Bookmark(ms) => {
                let at = snap_now(ms);
                let mut added = true;
                panel.set_show(&show_id(), |s| {
                    added = edits::add_bookmark(s, at).is_some();
                });
                if !added {
                    panel.refuse(format!("A bookmark is already at {}.", clock(at)));
                }
            }
            Click::BookmarkMenu(id) => bookmark_menu(&e, id),
            Click::Cut(id) => cut_menu(&e, id),
            Click::Keyframe(ms) => keyframe_menu(&e, snap_now(ms)),
        }
    };
    let context = move |e: leptos::ev::MouseEvent| {
        let Some(target) = target_of(&e) else {
            return;
        };
        let ms = snap_now(ms_at(f64::from(e.client_x())));
        if let Some(el) = closest(&target, "[data-bm]") {
            bookmark_menu(&e, el.get_attribute("data-bm").unwrap_or_default());
        } else if let Some(el) = closest(&target, "[data-cut]") {
            cut_menu(&e, el.get_attribute("data-cut").unwrap_or_default());
        } else if let Some(el) = closest(&target, "[data-clip], [data-loop]") {
            let id = el
                .get_attribute("data-clip")
                .or_else(|| el.get_attribute("data-loop"))
                .unwrap_or_default();
            selected.set(Some(id.clone()));
            let Some(menus) = menus else { return };
            let loop_id = el.get_attribute("data-loop");
            let mut entries = Vec::new();
            // An overlay or an A clip can stretch over the whole show in one step.
            if matches!(
                el.get_attribute("data-kind").as_deref(),
                Some("overlay" | "audio")
            ) && loop_id.is_none()
            {
                let clip = id.clone();
                entries.push(
                    Entry::item("Span the whole show", move || {
                        let mut spans = true;
                        panel.set_show(&show_id(), |s| {
                            spans = edits::span_show(s, &clip);
                        });
                        if !spans {
                            panel.refuse(
                                "It can't span the whole show: another clip on its track is in the way, or it already does.",
                            );
                        }
                    })
                    .disabled_if(locked_now()),
                );
                entries.push(Entry::Sep);
            }
            if let Some(l) = loop_id {
                entries.push(
                    Entry::item("Rename", move || renaming.set(Some(l.clone())))
                        .disabled_if(locked_now()),
                );
            }
            entries.push(
                Entry::item("Delete", remove_selected)
                    .hint("Del")
                    .danger()
                    .disabled_if(locked_now()),
            );
            menus.open(&e, entries);
        } else if let Some(el) = closest(&target, "[data-track-head]") {
            track_menu(
                &e,
                el.get_attribute("data-track-head").unwrap_or_default(),
                el.get_attribute("data-kind").unwrap_or_default(),
                ms,
                true,
            );
        } else if let Some(el) = closest(&target, "[data-track]") {
            track_menu(
                &e,
                el.get_attribute("data-track").unwrap_or_default(),
                el.get_attribute("data-kind").unwrap_or_default(),
                ms,
                false,
            );
        }
    };
    let key = move |e: leptos::ev::KeyboardEvent| {
        if e.ctrl_key() || e.meta_key() || e.alt_key() || e.default_prevented() {
            return;
        }
        if target_of(&e).is_some_and(|t| closest(&t, "input, select, textarea").is_some()) {
            return;
        }
        let k = e.key();
        match k.as_str() {
            "Delete" | "Backspace" if selected.get_untracked().is_some() => remove_selected(),
            "Escape" => {
                if press.get_value().is_some() {
                    cancel();
                } else {
                    selected.set(None);
                }
            }
            _ => match Tool::from_key(&k) {
                Some(t) if !locked_now() => tool.set(t),
                _ => return,
            },
        }
        e.prevent_default();
        e.stop_propagation();
    };

    // Something dragged over the tracks: a scene from the Scenes panel or files.
    let hover = RwSignal::new(None::<Hover>);
    let last_over = StoredValue::new(0.0_f64);
    let content = NodeRef::<leptos::html::Div>::new();
    let hover_at = move |x: f64, y: f64| -> Option<Hover> {
        let el = crate::host::element_at(x, y)?;
        let inside = content
            .get_untracked()
            .is_some_and(|c| c.contains(Some(&el)));
        if !inside {
            return None;
        }
        let body = closest(&el, "[data-track]")?;
        Some(Hover {
            track: body.get_attribute("data-track")?,
            at_ms: ms_at(x),
            slot: closest(&el, ".show-slot").is_some(),
        })
    };
    let track_of = move |id: &str| {
        show.with_untracked(|s| {
            s.as_ref()
                .and_then(|s| s.tracks.iter().find(|t| t.id == id).cloned())
        })
    };
    let folder = move || crate::project::collection_folder(open_project);
    let drop_files = move |paths: Vec<String>, h: Hover| {
        if locked_now() {
            panel.refuse("The show is locked: unlock it to edit.");
            return;
        }
        if paths.is_empty() {
            panel.refuse(
                "Nothing to place: the drop carried no file paths. Drag files from a file \
                 manager, or drop them on the desktop app's window.",
            );
            return;
        }
        if let Some(track) = track_of(&h.track) {
            let at = snap_now(h.at_ms);
            drops::place_files(
                &panel,
                &show_id(),
                &track,
                &paths,
                at,
                h.slot,
                folder().as_deref(),
            );
        }
    };
    Effect::new(move |_| {
        let Some(c) = panel.carry.get() else {
            if hover.with_untracked(Option::is_some) {
                hover.set(None);
            }
            return;
        };
        let h = hover_at(c.x, c.y);
        if !c.dropped {
            if hover.get_untracked() != h {
                hover.set(h);
            }
            return;
        }
        panel.carry.set(None);
        hover.set(None);
        let Some(h) = h else { return };
        if locked_now() {
            panel.refuse("The show is locked: unlock it to edit.");
            return;
        }
        let Some(track) = track_of(&h.track) else {
            return;
        };
        let at = snap_now(h.at_ms);
        if let Some(source) = c.source {
            // A source's audio: on the A track it was let go on, or a new one.
            let on = matches!(track.kind, TrackKind::Audio(_)).then_some(track.id.as_str());
            let mut fits = true;
            panel.set_show(&show_id(), |s| {
                fits = edits::place_source_audio(
                    s,
                    on,
                    source.as_str().into(),
                    at,
                    drops::DEFAULT_SCENE_MS,
                    false,
                )
                .is_some();
            });
            if !fits {
                panel.refuse(format!(
                    "{}'s audio doesn't fit on that track there.",
                    c.name
                ));
            }
            return;
        }
        drops::place_scene(&panel, &show_id(), &track, (&c.scene, &c.name), at, h.slot);
    });
    // A browser's drag of files clears itself when the drag-overs stop coming.
    Effect::new(move |_| {
        let now = panel.clock.get();
        if panel.carry.with_untracked(Option::is_none)
            && hover.with_untracked(Option::is_some)
            && now - last_over.get_value() > 250.0
        {
            hover.set(None);
        }
    });
    let drag_over = move |e: leptos::ev::DragEvent| {
        e.prevent_default();
        last_over.set_value(crate::store::now_ms());
        let h = hover_at(f64::from(e.client_x()), f64::from(e.client_y()));
        if hover.get_untracked() != h {
            hover.set(h);
        }
    };
    let dropped = move |e: leptos::ev::DragEvent| {
        e.prevent_default();
        let h = hover_at(f64::from(e.client_x()), f64::from(e.client_y()));
        hover.set(None);
        let paths = super::drop::dropped_paths(e.as_ref());
        if let Some(h) = h {
            drop_files(paths, h);
        }
    };
    if let Some(handle) = crate::host::on_file_drop(move |d| {
        use crate::host::DropPhase;
        let over = content
            .get_untracked()
            .is_some_and(|c| d.lands_on(c.as_ref()));
        match d.phase {
            DropPhase::Over if over => {
                last_over.set_value(crate::store::now_ms());
                hover.set(hover_at(d.x, d.y));
            }
            DropPhase::Drop if over => {
                let h = hover_at(d.x, d.y);
                hover.set(None);
                if let Some(h) = h {
                    drop_files(d.paths, h);
                }
            }
            _ => hover.set(None),
        }
    }) {
        on_cleanup(move || handle.remove());
    }

    let ticks = move || {
        let v = viewport.get();
        v.ticks(width.get())
            .into_iter()
            .map(|t| {
                let left = (f64::from(t) - v.start_ms) / v.span_ms * 100.0;
                view! { <span class="show-tick" style:left=format!("{left:.4}%")>{clock(t)}</span> }
            })
            .collect_view()
    };
    // Renaming a bookmark, a loop or a track: a name field over its flag, bar or head.
    let rename_field = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if renaming.get().is_some() {
            request_animation_frame(move || {
                if let Some(el) = rename_field.get_untracked() {
                    let _ = el.focus();
                    el.select();
                }
            });
        }
    });
    let finish_rename = move |keep: bool| {
        let Some(id) = renaming.get_untracked() else {
            return;
        };
        renaming.set(None);
        if keep && let Some(el) = rename_field.get_untracked() {
            let name = el.value();
            if !name.trim().is_empty() {
                panel.set_show(&show_id(), |s| {
                    edits::rename(s, &id, &name);
                });
            }
        }
    };
    let rows = move || {
        let v = viewport.get();
        let sel = selected.get();
        let over = hover.get();
        let naming = renaming.get();
        let lift = lifting.get();
        show.with(|s| {
            let Some(s) = s else {
                return Vec::new();
            };
            let program_end = s.program().last().map_or(0, ClipView::end_ms);
            let slot = over
                .as_ref()
                .and_then(|_| geom::drop_slot_pct(v, program_end));
            s.tracks
                .iter()
                .enumerate()
                .map(|(n, t)| {
                    let is_over = over.as_ref().is_some_and(|h| h.track == t.id);
                    track_row(
                        t,
                        n.checked_sub(1).map(|p| s.tracks[p].kind),
                        v,
                        sel.as_deref(),
                        (is_over, slot),
                        lift.as_ref().map_or((false, false), |(id, over)| {
                            (*id == t.id, over.as_deref() == Some(t.id.as_str()))
                        }),
                        (naming.as_deref() == Some(t.id.as_str())).then(|| {
                            let name = if t.name.is_empty() {
                                t.kind.title().to_owned()
                            } else {
                                t.name.clone()
                            };
                            view! {
                                <input class="show-track-rename" aria-label="Track name"
                                    node_ref=rename_field value=name
                                    on:pointerdown=|e| e.stop_propagation()
                                    on:keydown=move |e| {
                                        match e.key().as_str() {
                                            "Enter" => finish_rename(true),
                                            "Escape" => finish_rename(false),
                                            _ => return,
                                        }
                                        e.prevent_default();
                                        e.stop_propagation();
                                    }
                                    on:blur=move |_| finish_rename(true) />
                            }
                            .into_any()
                        }),
                    )
                })
                .collect::<Vec<_>>()
        })
    };
    let has_loops = move || show.with(|s| s.as_ref().is_some_and(|s| !s.loops.is_empty()));

    // What lies over the rows: loops, bookmark lines and their flags, the armed spot, the
    // play head and the dimmed part already played.
    let loops = move || {
        let v = viewport.get();
        let sel = selected.get();
        show.with(|s| {
            s.iter()
                .flat_map(|s| &s.loops)
                .filter_map(|l| {
                    let (left, w) = span_pct(v, l.start_ms, l.end_ms)?;
                    let right = 100.0 - left - w;
                    // Its sides straddle its ends, but a start right at the view's left
                    // edge keeps its side inside the view.
                    let left = if left >= 0.0 {
                        format!("max(0px, calc({left:.4}% - 3px))")
                    } else {
                        format!("calc({left:.4}% - 3px)")
                    };
                    let on = sel.as_deref() == Some(l.id.as_str());
                    Some(view! {
                        <div class="show-loop" class:show-sel=on
                            style:left=left
                            style:right=format!("calc({right:.4}% - 3px)")>
                            <em data-loop=l.id.clone()
                                title="Drag to move the loop, its ends to resize it">
                                "⟲ Loop · "{l.name.clone()}" · repeats until you leave"
                            </em>
                        </div>
                    })
                })
                .collect_view()
        })
    };
    let bookmarks = move || {
        let v = viewport.get();
        let w = width.get();
        let sel = selected.get();
        show.with(|s| {
            let Some(s) = s else {
                return Vec::new();
            };
            let shown: Vec<_> = s
                .bookmarks
                .iter()
                .filter_map(|b| {
                    let pct = at_pct(v, b.at_ms)?;
                    let seeks = geom::seeks(s, b.at_ms);
                    Some((b, pct, seeks))
                })
                .collect();
            let flags: Vec<(f64, f64)> = shown
                .iter()
                .map(|(b, pct, seeks)| (pct / 100.0 * w, geom::flag_px(b, *seeks)))
                .collect();
            let places = geom::place_flags(&flags, w);
            shown
                .into_iter()
                .zip(places)
                .map(|((b, pct, seeks), place)| {
                    let title = format!(
                        "{} at {}{}{}",
                        b.name,
                        clock(b.at_ms),
                        if b.pausing { " · pauses" } else { "" },
                        if seeks {
                            " · a jump here seeks into the file"
                        } else {
                            ""
                        }
                    );
                    let on = sel.as_deref() == Some(b.id.as_str());
                    view! {
                        <div class="show-bm" class:pausing=b.pausing class:edge=pct <= 0.0
                            data-bm=b.id.clone() style:left=format!("{pct:.4}%")></div>
                        <em class="show-flag" class:flip=place.flipped class:show-sel=on
                            data-bm=b.id.clone()
                            style:left=format!("{:.1}px", place.left)
                            style:max-width=format!("{:.1}px", place.max_width)
                            title=title>
                            {b.pausing.then_some("❚❚ ")}{b.name.clone()}
                            {b.key.clone().map(|k| view! { <kbd>{k}</kbd> })}
                            {seeks.then(|| view! { <small>"seeks"</small> })}
                        </em>
                    }
                    .into_any()
                })
                .collect::<Vec<_>>()
        })
    };
    let rename_box = move || {
        let id = renaming.get()?;
        let (at, name, is_loop) = show.with(|s| {
            let s = s.as_ref()?;
            s.bookmarks
                .iter()
                .find(|b| b.id == id)
                .map(|b| (b.at_ms, b.name.clone(), false))
                .or_else(|| {
                    s.loops
                        .iter()
                        .find(|l| l.id == id)
                        .map(|l| (l.start_ms, l.name.clone(), true))
                })
        })?;
        let left = at_pct(viewport.get(), at).unwrap_or(0.0);
        Some(view! {
            <div class="show-rename" class:on-loop=is_loop style:left=format!("{left:.4}%")>
                <input aria-label=if is_loop { "Loop name" } else { "Bookmark name" }
                    node_ref=rename_field value=name
                    on:keydown=move |e| {
                        match e.key().as_str() {
                            "Enter" => finish_rename(true),
                            "Escape" => finish_rename(false),
                            _ => return,
                        }
                        e.prevent_default();
                        e.stop_propagation();
                    }
                    on:blur=move |_| finish_rename(true) />
            </div>
        })
    };
    let armed = move || {
        let at = panel.run.with(|r| r.armed_ms).filter(|_| running.get())?;
        let left = at_pct(viewport.get(), at)?;
        Some(view! {
            <div class="show-armed" style:left=format!("{left:.4}%")>
                <em>"Armed · "{clock(at)}</em>
            </div>
        })
    };
    let head = move || {
        let cursor = panel.cursor.get();
        if !running.get() && cursor.is_none() {
            return None;
        }
        let at = panel.head();
        let v = viewport.get();
        let played = ((f64::from(at) - v.start_ms) / v.span_ms * 100.0).clamp(0.0, 100.0);
        let text = if running.get() {
            panel
                .run
                .with(|r| geom::head_text(r, at, panel.clock.get()))
        } else {
            format!("{} · Play starts here", clock(at))
        };
        let line = at_pct(v, at).map(|left| {
            // The flag hangs left of the line where it would run off the right edge.
            let flip =
                left / 100.0 * width.get() + text.chars().count() as f64 * 6.4 + 16.0 > width.get();
            view! {
                <div class="show-head" style:left=format!("{left:.4}%")>
                    <em class:flip=flip>{text}</em>
                </div>
            }
        });
        Some(view! {
            <div class="show-played" style:width=format!("{played:.4}%")></div>
            {line}
        })
    };
    let tip_view = move || {
        tip.get().map(|t| {
            view! {
                <div class="show-tip" class:refused=t.refused
                    style:left=format!("{}px", t.x + 14)
                    style:top=format!("{}px", t.y - 34)>{t.text}</div>
            }
        })
    };

    let wheel = move |e: leptos::ev::WheelEvent| {
        let Some(el) = scroller.get_untracked() else {
            return;
        };
        let rect = el.get_bounding_client_rect();
        let x = f64::from(e.client_x()) - rect.left() - HEAD_PX;
        let next = geom::wheeled(
            viewport.get_untracked(),
            (e.delta_x(), e.delta_y()),
            e.ctrl_key() || e.meta_key(),
            e.shift_key(),
            x,
            width.get_untracked(),
        );
        if let Some(next) = next {
            e.prevent_default();
            e.stop_propagation();
            viewport.set(next);
        }
    };

    view! {
        <div class="show-edit" class:locked=locked tabindex="-1" node_ref=root on:keydown=key>
            <div class="show-note">{AUDIO_NOTE}</div>
            {bar}
            <div class="show-edit-body">
                {tools}
                <div class="show-timeline">
                    <div class="show-scroll" node_ref=scroller on:wheel=wheel>
                        <div class=move || format!("show-content show-using-{}", tool.get().class())
                            class:has-loops=has_loops
                            class:dragging-in=move || hover.with(Option::is_some)
                            node_ref=content
                            on:pointerdown=down on:pointermove=moving on:pointerup=up
                            on:pointercancel=move |_| cancel()
                            on:click=clicked on:contextmenu=context
                            on:dragover=drag_over on:drop=dropped>
                            <div class="show-ruler-row">
                                <div class="show-corner"></div>
                                <div class="show-ruler"
                                    class:scrubs=move || !matches!(tool.get(), Tool::Loop | Tool::Bookmark)
                                    title=move || match tool.get() {
                                        Tool::Loop => "Drag along the ruler to make a loop",
                                        Tool::Bookmark => "Click to add a bookmark",
                                        _ if running.get() => "Click or drag to move the play head (Shift: no snapping)",
                                        _ => "Click or drag to set where Play starts (Shift: no snapping)",
                                    }>{ticks}</div>
                            </div>
                            {rows}
                            <div class="show-flag-row"></div>
                            {move || has_loops().then(|| view! { <div class="show-loop-row"></div> })}
                            <div class="show-marks">
                                {loops}
                                {bookmarks}
                                {rename_box}
                                {armed}
                                {head}
                            </div>
                        </div>
                    </div>
                    <zoom::ZoomBar show=show />
                </div>
            </div>
            <div class="show-foot">{HINT}</div>
            {tip_view}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_tool_has_its_key() {
        for t in Tool::ALL {
            let key = t.name().split('(').nth(1).map(|k| k.trim_end_matches(')'));
            assert_eq!(key.and_then(Tool::from_key), Some(t), "{t:?}");
        }
        assert_eq!(Tool::from_key("v"), Some(Tool::Select));
        assert_eq!(Tool::from_key("q"), None);
    }
}
