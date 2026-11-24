//! The Show panel's Edit view: the timeline. Its bar (the show picker, Edit / Run, Stop
//! show, Back, Continue and what it will do, Rehearse, Lock, the status, + Overlay track and
//! Fit show), the tool strip, the ruler with the play head's flag, the tracks, the
//! bookmark lines with their flags along the bottom, the loops' bars under them, and the
//! zoom bar over the whole show.
//!
//! This first version draws the show and moves around it: Ctrl+wheel zooms about the
//! pointer, Shift+wheel scrolls, and the zoom bar does both ([`zoom`]). While the show
//! runs, a click on the ruler arms that spot for the next Continue; it never jumps. The
//! tools in the strip are picked but do nothing yet. Where things go is worked out in
//! [`geom`], as pure functions.
//!
//! Docs: crates/jstream-ui/README.md#design

pub mod geom;
pub mod zoom;

use super::model::{ClipKind, ClipView, RunPhase, ShowView, TrackKind, TrackView};
use super::time::{MIN_SPAN_MS, Viewport, clock};
use super::{Command, ModeSwitch, ShowPanel, status};
use geom::{HEAD_PX, at_pct, span_pct};
use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;

/// The footer's hint.
const HINT: &str = "Drag a scene, video, image or audio file onto a track · drag a clip edge \
                    to trim · click a cut badge for its transition · Bookmark tool (B) · Loop \
                    tool (L): drag on the ruler · zoom bar: drag an end to zoom";

/// A tool in the strip at the Edit view's left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// Pick and move clips.
    #[default]
    Select,
    /// Drag a clip's edge to change its length.
    Trim,
    /// Cut a clip in two.
    Split,
    /// Place a show bookmark (B).
    Bookmark,
    /// Drag a loop on the ruler (L).
    Loop,
    /// Set the transition on a cut.
    Transition,
    /// Place a keyframe.
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

    /// Its name, in its tooltip.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Trim => "Trim",
            Tool::Split => "Split",
            Tool::Bookmark => "Bookmark (B)",
            Tool::Loop => "Loop (L)",
            Tool::Transition => "Transition",
            Tool::Keyframe => "Keyframe",
        }
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

/// One clip of a track in the part of the show `view` shows, or nothing when it is out of
/// view. `n` is its place on its track, for the program colours.
fn clip_view(kind: TrackKind, n: usize, clip: &ClipView, view: Viewport) -> Option<AnyView> {
    let marker = clip.len_ms == 0;
    if marker {
        let left = format!("{:.4}%", at_pct(view, clip.start_ms)?);
        return Some(match clip.kind {
            ClipKind::Keyframe(_) => view! {
                <div class="show-kf" class:missing=clip.missing style:left=left title=name(clip)>
                    <s></s>{name(clip)}
                </div>
            }
            .into_any(),
            _ => view! {
                <div class="show-ev" class:missing=clip.missing style:left=left title=name(clip)>
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
    Some(match (kind, &clip.kind) {
        (TrackKind::Program, _) => view! {
            <div class=format!("show-clip show-prog show-clip-c{}", n % geom::PROGRAM_COLOURS)
                class:missing=clip.missing style:left=left style:width=width title=title>
                <b>{name(clip)}{badge}</b>
                <span>{clip.detail.clone()}</span>
            </div>
        }
        .into_any(),
        (_, ClipKind::Sequence(_)) => view! {
            <div class="show-seq" class:missing=clip.missing style:left=left style:width=width title=title>
                <svg viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
                    <path d=geom::curve_path(&clip.curve) vector-effect="non-scaling-stroke" />
                </svg>
                <b>"▶ "{name(clip)}{badge}</b>
            </div>
        }
        .into_any(),
        (_, ClipKind::Audio(_)) => {
            let fades = clip.fades.unwrap_or((0, 0));
            view! {
                <div class="show-au" class:missing=clip.missing style:left=left style:width=width title=title>
                    <svg viewBox="0 0 1000 100" preserveAspectRatio="none" aria-hidden="true">
                        <path class="fill" d=geom::fade_path(clip.len_ms, fades, true) />
                        <path class="line" d=geom::fade_path(clip.len_ms, fades, false) vector-effect="non-scaling-stroke" />
                    </svg>
                    <b>{name(clip)}{badge}</b>
                    <span>{clip.detail.clone()}</span>
                </div>
            }
            .into_any()
        }
        _ => view! {
            <div class="show-clip show-ov" class:missing=clip.missing style:left=left style:width=width title=title>
                <b>{name(clip)}{badge}</b>
                <span>{clip.detail.clone()}</span>
            </div>
        }
        .into_any(),
    })
}

/// The transition badges on program's cuts in view: "⧓ Fade 0.5 s" over each cut.
fn cuts(track: &TrackView, view: Viewport) -> Vec<AnyView> {
    track
        .clips
        .iter()
        .filter_map(|c| {
            let label = c.transition_in.clone()?;
            let left = at_pct(view, c.start_ms)?;
            Some(
                view! {
                    <div class="show-cut" style:left=format!("{left:.4}%")
                        title=format!("Transition into {}: {label}", c.label)>
                        <i>"⧓"</i>{label.clone()}
                    </div>
                }
                .into_any(),
            )
        })
        .collect()
}

/// One track's row: its head and its clips.
fn track_row(track: &TrackView, previous: Option<TrackKind>, view: Viewport) -> AnyView {
    let kind = track.kind;
    let mut body: Vec<AnyView> = track
        .clips
        .iter()
        .enumerate()
        .filter_map(|(n, c)| clip_view(kind, n, c, view))
        .collect();
    if kind == TrackKind::Program {
        body.extend(cuts(track, view));
    }
    let muted = track.muted;
    let locked = track.locked;
    view! {
        <div class=format!("show-track show-track-{}", kind.class())
            class:show-group=geom::starts_group(previous, kind)
            class:muted=muted
            style:height=format!("{}px", geom::row_px(kind))>
            <div class="show-track-head">
                <b><span class="show-tag">{kind.tag()}</span>{kind.title()}</b>
                <span class="show-track-icons">
                    <i class="show-mute" class:on=muted title=if muted { "Muted" } else { "Plays" }>"◉"</i>
                    <i class="show-lock" class:on=locked title=if locked { "Locked" } else { "Unlocked" }>"🔒"</i>
                </span>
            </div>
            <div class="show-track-body">{body}</div>
        </div>
    }
    .into_any()
}

/// The Edit view.
#[component]
pub fn EditView() -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let viewport = panel.viewport;
    let tool = RwSignal::new(Tool::Select);
    let show = Memo::new(move |_| panel.show());
    let running = Memo::new(move |_| panel.is_running());
    // The rows' width in px (the view less the track heads), measured with the clock's
    // tick so a dock resize follows.
    let width = RwSignal::new(0.0_f64);
    let scroller = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        panel.clock.track();
        if let Some(el) = scroller.get() {
            let w = (f64::from(el.client_width()) - HEAD_PX).max(0.0);
            if (w - width.get_untracked()).abs() > 0.5 {
                width.set(w);
            }
        }
    });
    // The view follows the show's length: the whole show at first, then kept as it is.
    Effect::new(move |_| {
        let len = show.with(|s| s.as_ref().map_or(0, ShowView::length_ms));
        let now = viewport.get_untracked();
        let next = if now.len_ms <= MIN_SPAN_MS {
            Viewport::fit(len)
        } else {
            now.with_len(len)
        };
        if next != now {
            viewport.set(next);
        }
    });

    let send = move |c: Command| panel.send(c);
    let show_id = move || show.with(|s| s.as_ref().map(|s| s.id.clone()).unwrap_or_default());
    let phase = move || panel.run.with(|r| r.phase.clone());
    let paused = move || matches!(phase(), RunPhase::Paused { .. });
    let continue_ = move |_| {
        if running.get_untracked() {
            send(Command::Continue);
        } else {
            send(Command::Play(show_id()));
        }
    };
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
        let text = panel
            .run
            .with(|r| show.with(|s| s.as_ref().map(|s| status(s, r, now))));
        let glyph = paused().then_some("❚❚ ");
        view! {
            <span class="show-state" class:idle=move || !running.get()>{glyph}{text}</span>
        }
    };
    let locked = move || show.with(|s| s.as_ref().is_some_and(|s| s.locked));
    let rehearsing = move || panel.run.with(|r| r.rehearsing);

    let bar = view! {
        <div class="show-bar show-edit-bar">
            <ModeSwitch />
            <button type="button" class="show-btn" disabled=move || !running.get()
                on:click=move |_| send(Command::Stop)>"■ Stop show"</button>
            <button type="button" class="show-btn" disabled=move || !running.get()
                title="Back to the previous pausing bookmark"
                on:click=move |_| send(Command::Back)>"⏮ Back"</button>
            <button type="button" class="show-continue"
                disabled=move || matches!(phase(), RunPhase::Transition)
                on:click=continue_>
                {move || if running.get() { "Continue ▸" } else { "Play ▸" }}
            </button>
            {move || panel.continue_key.get().map(|k| view! { <kbd class="show-keycap">{k}</kbd> })}
            {next}
            <span class="show-bar-gap"></span>
            <button type="button" class="show-btn" class:on=rehearsing
                aria-pressed=move || rehearsing().to_string()
                title="Run on preview, leaving program alone"
                on:click=move |_| send(Command::Rehearse(!rehearsing()))>"Rehearse"</button>
            <button type="button" class="show-btn" class:on=locked
                aria-pressed=move || locked().to_string()
                title=move || if locked() { "Editing is locked: unlock it" } else { "Lock the show against edits" }
                on:click=move |_| send(Command::Lock(show_id(), !locked()))>
                {move || if locked() { "🔒 Locked" } else { "🔓 Lock" }}
            </button>
            {state}
            <button type="button" class="show-btn"
                on:click=move |_| send(Command::AddOverlayTrack(show_id()))>"+ Overlay track"</button>
            <button type="button" class="show-btn"
                on:click=move |_| {
                    let len = show.with(|s| s.as_ref().map_or(0, ShowView::length_ms));
                    viewport.set(Viewport::fit(len));
                }>"Fit show"</button>
        </div>
    };

    let tools = view! {
        <div class="show-tools" role="toolbar" aria-label="Tools" aria-orientation="vertical">
            {Tool::ALL.into_iter().map(|t| view! {
                <button type="button" class=format!("show-tool show-tool-{}", t.class())
                    class:on=move || tool.get() == t
                    aria-pressed=move || (tool.get() == t).to_string()
                    title=t.name() aria-label=t.name()
                    on:click=move |_| tool.set(t)>{t.glyph()}</button>
            }).collect_view()}
        </div>
    };

    // A ruler click while the show runs arms that spot for the next Continue.
    let ruler_click = move |e: leptos::ev::MouseEvent| {
        if !running.get_untracked() {
            return;
        }
        let Some(el) = e
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        let rect = el.get_bounding_client_rect();
        let x = f64::from(e.client_x()) - rect.left();
        let ms = viewport.get_untracked().ms(x, rect.width()).max(0.0);
        send(Command::Arm(Some(ms as u32)));
    };
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
    let rows = move || {
        let v = viewport.get();
        show.with(|s| {
            let Some(s) = s else {
                return Vec::new();
            };
            s.tracks
                .iter()
                .enumerate()
                .map(|(n, t)| track_row(t, n.checked_sub(1).map(|p| s.tracks[p].kind), v))
                .collect::<Vec<_>>()
        })
    };
    let has_loops = move || show.with(|s| s.as_ref().is_some_and(|s| !s.loops.is_empty()));

    // What lies over the rows: loops, bookmark lines and their flags, the armed spot, the
    // play head and the dimmed part already played.
    let loops = move || {
        let v = viewport.get();
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
                    Some(view! {
                        <div class="show-loop"
                            style:left=left
                            style:right=format!("calc({right:.4}% - 3px)")>
                            <em>"⟲ Loop · "{l.name.clone()}" · repeats until you leave"</em>
                        </div>
                    })
                })
                .collect_view()
        })
    };
    let bookmarks = move || {
        let v = viewport.get();
        let w = width.get();
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
                        if seeks { " · a jump here seeks into the file" } else { "" }
                    );
                    view! {
                        <div class="show-bm" class:pausing=b.pausing class:edge=pct <= 0.0 style:left=format!("{pct:.4}%")></div>
                        <em class="show-flag" class:flip=place.flipped
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
        if !running.get() {
            return None;
        }
        let at = panel.head();
        let v = viewport.get();
        let played = ((f64::from(at) - v.start_ms) / v.span_ms * 100.0).clamp(0.0, 100.0);
        let text = panel
            .run
            .with(|r| geom::head_text(r, at, panel.clock.get()));
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
        <div class="show-edit">
            {bar}
            <div class="show-edit-body">
                {tools}
                <div class="show-timeline">
                    <div class="show-scroll" node_ref=scroller on:wheel=wheel>
                        <div class="show-content" class:has-loops=has_loops>
                            <div class="show-ruler-row">
                                <div class="show-corner"></div>
                                <div class="show-ruler" class:armable=move || running.get()
                                    title=move || if running.get() { "Click to arm this spot for the next Continue" } else { "" }
                                    on:click=ruler_click>{ticks}</div>
                            </div>
                            {rows}
                            <div class="show-flag-row"></div>
                            {move || has_loops().then(|| view! { <div class="show-loop-row"></div> })}
                            <div class="show-marks" aria-hidden="true">
                                {loops}
                                {bookmarks}
                                {armed}
                                {head}
                            </div>
                        </div>
                    </div>
                    <zoom::ZoomBar show=show />
                </div>
            </div>
            <div class="show-foot">{HINT}</div>
        </div>
    }
}
