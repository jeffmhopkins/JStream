//! The Show panel's Run view, for show time: its bar (the show picker, Edit / Run, Stop
//! show, Lock, Rehearse on preview, the Next monitor's Live / Still / Off and the status),
//! the Now and Next cards over a big Continue, the bookmarks as buttons in show order, the
//! clocks, what is up right now, and the tracks folded into one strip.
//!
//! What each part says is a pure function of the view-model ([`now`], [`next`], [`cards`],
//! [`clocks`], [`up_now`], [`strip`]), tested natively; the components only draw them and
//! send one [`Command`] per button.
//!
//! Docs: crates/jstream-ui/README.md#design

use super::model::{self, BookmarkView, ClipKind, ClipView, NextMonitor, RunPhase, ShowView};
use super::{Command, ModeSwitch, ShowPanel, time};
use leptos::prelude::*;

/// How many colours the strip's program clips cycle through, in clip order
/// (`show-seg-0` to `show-seg-4` in style.css).
pub const SEGMENT_COLOURS: usize = 5;

/// What a card says: a big title over one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    /// The big line.
    pub title: String,
    /// The line under it.
    pub line: String,
}

/// Whether what sits exactly at the play head has happened yet. Paused at a bookmark, what
/// starts there fires on Continue, so program still shows what came before.
fn settled(run: &model::RunView) -> bool {
    !matches!(run.phase, RunPhase::Paused { .. })
}

/// Whether something starting at `start` has started at `at`.
fn started(start: u32, at: u32, inclusive: bool) -> bool {
    start < at || (inclusive && start == at)
}

/// The program clip on air at `at`, and the keyframe of it applied by then.
fn on_program(show: &ShowView, at: u32, inclusive: bool) -> (Option<&ClipView>, Option<&ClipView>) {
    let program = show.program();
    let clip = program
        .iter()
        .take_while(|c| started(c.start_ms, at, inclusive))
        .last()
        .or_else(|| program.first());
    let key = clip.and_then(|clip| {
        keyframes(show)
            .filter(|k| {
                k.start_ms >= clip.start_ms && k.start_ms < clip.end_ms().max(clip.start_ms + 1)
            })
            .take_while(|k| started(k.start_ms, at, inclusive))
            .last()
    });
    (clip, key)
}

/// Every keyframe marker in the show, in time order.
fn keyframes(show: &ShowView) -> impl Iterator<Item = &ClipView> {
    show.tracks
        .iter()
        .filter(|t| t.kind == model::TrackKind::Keyframes)
        .flat_map(|t| &t.clips)
}

/// "Main · Wide": a program clip and its keyframe.
fn scene_and_key(clip: Option<&ClipView>, key: Option<&ClipView>) -> String {
    match (clip, key) {
        (Some(c), Some(k)) => format!("{} · {}", c.label, k.label),
        (Some(c), None) => c.label.clone(),
        (None, Some(k)) => k.label.clone(),
        (None, None) => "—".to_owned(),
    }
}

/// ", with a Move" from a transition such as "Move 0.8 s"; ", on a cut" for a cut.
fn with_transition(transition: Option<&str>) -> String {
    match transition.and_then(|t| t.split_whitespace().next()) {
        None => String::new(),
        Some(w) if w.eq_ignore_ascii_case("cut") => ", on a cut".to_owned(),
        Some(w) => format!(", with a {w}"),
    }
}

/// The NOW card: what is on program and what the runner is doing.
pub fn now(show: &ShowView, run: &model::RunView, head: u32, now: f64) -> Card {
    let (clip, key) = on_program(show, head, settled(run));
    let title = match &run.off_script {
        Some(scene) => scene.clone(),
        None => scene_and_key(clip, key),
    };
    let mut line = match &run.phase {
        RunPhase::Stopped => "Stopped".to_owned(),
        RunPhase::Transition => "In transition".to_owned(),
        RunPhase::Paused { bookmark, since } => {
            let name = show
                .bookmarks
                .iter()
                .find(|b| &b.id == bookmark)
                .map_or("a bookmark", |b| b.name.as_str());
            let waited = (now - since).max(0.0) as u32;
            format!("Paused at bookmark {name} for {}", time::clock(waited))
        }
        RunPhase::Playing => match &run.looping {
            Some((id, pass)) => {
                let name = show
                    .loops
                    .iter()
                    .find(|l| &l.id == id)
                    .map_or("a loop", |l| l.name.as_str());
                format!("Looping {name} · pass {pass}")
            }
            None => match clip {
                Some(c) => format!(
                    "Playing · {} left in this clip",
                    time::clock(c.end_ms().saturating_sub(head))
                ),
                None => "Playing".to_owned(),
            },
        },
    };
    if run.off_script.is_some() {
        line.push_str(" · off script");
    }
    if run.rehearsing {
        line.push_str(" · rehearsing on preview");
    }
    Card { title, line }
}

/// What starts at `at` off program, in words: "Show Title", "Start Ticker", "Play Mic".
fn fires_at(show: &ShowView, at: u32) -> Vec<String> {
    show.tracks
        .iter()
        .filter(|t| !t.muted)
        .flat_map(|t| &t.clips)
        .filter(|c| c.start_ms == at)
        .filter_map(|c| match &c.kind {
            ClipKind::Scene(_) if show.program().contains(c) => None,
            ClipKind::Scene(_) => Some(format!("Show {}", c.label)),
            ClipKind::Sequence(_) => Some(format!("Start {}", c.label)),
            ClipKind::Event(what) => Some(what.clone()),
            ClipKind::Audio(_) => Some(format!("Play {}", c.label)),
            ClipKind::Keyframe(_) => None,
        })
        .collect()
}

/// The NEXT ON CONTINUE card: where Continue takes the show and what fires there.
pub fn next(show: &ShowView, run: &model::RunView, head: u32) -> Card {
    if let Some(armed) = run.armed_ms {
        let (clip, key) = on_program(show, armed, true);
        return Card {
            title: scene_and_key(clip, key),
            line: format!("From the armed spot at {}", time::clock(armed)),
        };
    }
    match run.phase {
        RunPhase::Stopped => {
            let (clip, key) = on_program(show, 0, true);
            Card {
                title: scene_and_key(clip, key),
                line: "Starts the show from 0:00".to_owned(),
            }
        }
        RunPhase::Playing | RunPhase::Transition => match show.next_pause(head) {
            Some(b) => Card {
                title: b.name.clone(),
                line: format!(
                    "Goes to bookmark {} at {} and waits",
                    b.name,
                    time::clock(b.at_ms)
                ),
            },
            None => Card {
                title: "End of show".to_owned(),
                line: "No pausing bookmark ahead".to_owned(),
            },
        },
        RunPhase::Paused { .. } => {
            let cut = show.program().iter().find(|c| c.start_ms == head);
            let key = keyframes(show).find(|k| k.start_ms == head);
            let (title, mut line) = match (cut, key) {
                (Some(c), k) => (
                    scene_and_key(Some(c), k),
                    format!(
                        "Scene {}{}",
                        c.label,
                        with_transition(c.transition_in.as_deref())
                    ),
                ),
                (None, Some(k)) => (
                    k.label.clone(),
                    format!(
                        "Keyframe {}{}",
                        k.label,
                        with_transition(k.transition_in.as_deref())
                    ),
                ),
                (None, None) => {
                    let (clip, key) = on_program(show, head, true);
                    (scene_and_key(clip, key), "Plays on".to_owned())
                }
            };
            let fires = fires_at(show, head);
            if !fires.is_empty() {
                line.push_str(" · then ");
                line.push_str(&fires.join(", "));
            }
            Card { title, line }
        }
    }
}

/// The tag at the foot of a bookmark card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tag {
    /// The show waits here now, for this long (ms).
    Here(u32),
    /// A loop starts here.
    Loop,
    /// The show stops here until Continue.
    Pauses,
    /// The show plays through it.
    PlaysThrough,
}

impl Tag {
    /// The tag's text: "❚❚ HERE · 2:14", "⟲ LOOP", "❚❚ PAUSES", "PLAYS THROUGH".
    pub fn label(&self) -> String {
        match self {
            Tag::Here(waited) => format!("❚❚ HERE · {}", time::clock(*waited)),
            Tag::Loop => "⟲ LOOP".to_owned(),
            Tag::Pauses => "❚❚ PAUSES".to_owned(),
            Tag::PlaysThrough => "PLAYS THROUGH".to_owned(),
        }
    }

    /// Its class suffix in style.css (`show-tag-<class>`).
    pub fn class(&self) -> &'static str {
        match self {
            Tag::Here(_) | Tag::Pauses => "pause",
            Tag::Loop => "loop",
            Tag::PlaysThrough => "through",
        }
    }
}

/// One bookmark button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkCard {
    /// The bookmark's ID, for [`Command::GoTo`].
    pub id: String,
    /// Its place in show order, from 1: "go to bookmark 1..8" counts these.
    pub number: usize,
    /// Its name.
    pub name: String,
    /// What is on program there: "Main · keyframe Close", "intro.mp4 · 1:00".
    pub scene: String,
    /// Where it is, as the clocks write it.
    pub at: String,
    /// The key bound to it, as a key cap.
    pub key: Option<String>,
    /// The tag at its foot.
    pub tag: Tag,
    /// Whether the show waits here now (the yellow card).
    pub here: bool,
    /// Whether a loop starts here (the violet top edge).
    pub loop_start: bool,
    /// Whether the play head is past it (dimmed).
    pub past: bool,
}

/// What is on program at a bookmark, for its card.
fn bookmark_scene(show: &ShowView, b: &BookmarkView) -> String {
    let (clip, key) = on_program(show, b.at_ms, true);
    let Some(clip) = clip else {
        return String::new();
    };
    let mut s = clip.label.clone();
    if clip.timed {
        s.push_str(&format!(" · {}", time::clock(clip.len_ms)));
    }
    if let Some(k) = key {
        s.push_str(&format!(" · keyframe {}", k.label));
    }
    s
}

/// The bookmark buttons, in show order.
pub fn cards(show: &ShowView, run: &model::RunView, head: u32, now: f64) -> Vec<BookmarkCard> {
    show.bookmarks
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let waited = match &run.phase {
                RunPhase::Paused { bookmark, since } if bookmark == &b.id => {
                    Some((now - since).max(0.0) as u32)
                }
                _ => None,
            };
            let loop_start = show.loops.iter().any(|l| l.start_ms == b.at_ms);
            let tag = match waited {
                Some(w) => Tag::Here(w),
                None if loop_start => Tag::Loop,
                None if b.pausing => Tag::Pauses,
                None => Tag::PlaysThrough,
            };
            BookmarkCard {
                id: b.id.clone(),
                number: i + 1,
                name: b.name.clone(),
                scene: bookmark_scene(show, b),
                at: time::clock(b.at_ms),
                key: b.key.clone(),
                here: waited.is_some(),
                past: waited.is_none() && run.show.is_some() && b.at_ms < head,
                tag,
                loop_start,
            }
        })
        .collect()
}

/// The CLOCKS card's four readings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clocks {
    /// The play head: "5:30".
    pub show: String,
    /// How long the stream has been live, or "—".
    pub live: String,
    /// What is left of the program clip on air, or "—".
    pub left: String,
    /// "Against plan (32:00)", or "Against plan" for a show with no plan.
    pub plan_label: String,
    /// The projected finish against the plan, "+2:14" (late) or "-0:30" (early), or "—".
    pub against: String,
    /// Whether the show runs late.
    pub late: bool,
}

/// The CLOCKS card. Against plan projects the finish: the time the show has run, plus what
/// is left of it, less the planned length.
pub fn clocks(show: &ShowView, run: &model::RunView, head: u32) -> Clocks {
    let (clip, _) = on_program(show, head, true);
    let left = clip
        .filter(|c| run.show.is_some() && c.end_ms() > head)
        .map_or_else(|| "—".to_owned(), |c| time::clock(c.end_ms() - head));
    let against = show.planned_ms.zip(run.elapsed_ms).map(|(plan, ran)| {
        i64::from(ran) + i64::from(show.length_ms().saturating_sub(head)) - i64::from(plan)
    });
    Clocks {
        show: time::clock(head),
        live: run.live_ms.map_or_else(|| "—".to_owned(), time::clock),
        left,
        plan_label: show.planned_ms.map_or_else(
            || "Against plan".to_owned(),
            |p| format!("Against plan ({})", time::clock(p)),
        ),
        against: against.map_or_else(|| "—".to_owned(), time::signed),
        late: against.is_some_and(|d| d > 0),
    }
}

/// One row of UP RIGHT NOW.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpItem {
    /// What it is: "Lower third · Host".
    pub name: String,
    /// Its track and time left: "overlay V3 · 0:54 left", "sequence · looping", "audio A1".
    pub note: String,
}

/// What is up right now off program: overlays, sequences and audio clips spanning the play
/// head, in that order. A clip that runs to the show's end has no time left to show; a
/// sequence with a detail ("looping") shows that instead.
pub fn up_now(show: &ShowView, run: &model::RunView, head: u32) -> Vec<UpItem> {
    let inclusive = settled(run);
    let end = show.length_ms();
    let mut rows: Vec<((u8, u8), UpItem)> = show
        .tracks
        .iter()
        .filter(|t| !t.muted)
        .filter_map(|t| match t.kind {
            model::TrackKind::Overlay(n) => Some(((0, n), format!("overlay V{n}"), t)),
            model::TrackKind::Sequences(n) => Some(((1, n), "sequence".to_owned(), t)),
            model::TrackKind::Audio(n) => Some(((2, n), format!("audio A{n}"), t)),
            _ => None,
        })
        .flat_map(|(rank, what, t)| {
            t.clips
                .iter()
                .filter(move |c| {
                    c.len_ms > 0 && started(c.start_ms, head, inclusive) && head < c.end_ms()
                })
                .map(move |c| {
                    let tail = if matches!(c.kind, ClipKind::Sequence(_)) && !c.detail.is_empty() {
                        format!(" · {}", c.detail)
                    } else if c.end_ms() < end {
                        format!(" · {} left", time::clock(c.end_ms() - head))
                    } else {
                        String::new()
                    };
                    (
                        rank,
                        UpItem {
                            name: c.label.clone(),
                            note: format!("{what}{tail}"),
                        },
                    )
                })
        })
        .collect();
    rows.sort_by_key(|(rank, _)| *rank);
    rows.into_iter().map(|(_, row)| row).collect()
}

/// One program clip on the strip.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// Its name.
    pub label: String,
    /// Where it starts, in % of the strip.
    pub left: f64,
    /// How wide it is, in % of the strip.
    pub width: f64,
    /// Which of the [`SEGMENT_COLOURS`] it takes.
    pub colour: usize,
}

/// The folded strip: where everything sits, in % of its width.
#[derive(Clone, Debug, PartialEq)]
pub struct Strip {
    /// The program clips.
    pub segments: Vec<Segment>,
    /// The bookmark lines: where, and whether pausing (dashed).
    pub marks: Vec<(f64, bool)>,
    /// The loops' outlines: left and width.
    pub loops: Vec<(f64, f64)>,
    /// The play head.
    pub head: f64,
    /// The armed spot, if any.
    pub armed: Option<f64>,
}

/// The folded strip for `show`, with the play head at `head`.
pub fn strip(show: &ShowView, head: u32, armed: Option<u32>) -> Strip {
    let len = f64::from(show.length_ms().max(1));
    let pct = |ms: u32| (f64::from(ms) / len * 100.0).clamp(0.0, 100.0);
    Strip {
        segments: show
            .program()
            .iter()
            .enumerate()
            .map(|(i, c)| Segment {
                label: c.label.clone(),
                left: pct(c.start_ms),
                width: pct(c.end_ms()) - pct(c.start_ms),
                colour: i % SEGMENT_COLOURS,
            })
            .collect(),
        marks: show
            .bookmarks
            .iter()
            .map(|b| (pct(b.at_ms), b.pausing))
            .collect(),
        loops: show
            .loops
            .iter()
            .map(|l| (pct(l.start_ms), pct(l.end_ms) - pct(l.start_ms)))
            .collect(),
        head: pct(head),
        armed: armed.map(pct),
    }
}

/// The show time at a fraction `f` (0..1) across the strip.
pub fn strip_ms(show: &ShowView, f: f64) -> u32 {
    (f.clamp(0.0, 1.0) * f64::from(show.length_ms())).round() as u32
}

/// A % of the strip as a CSS length, for a `style:` binding (the page's CSP refuses a
/// `style` attribute).
fn pc(x: f64) -> String {
    format!("{x:.3}%")
}

/// A key cap, or a chip for a longer binding ("MIDI note 36 · ch 10").
fn binding(key: String) -> AnyView {
    if key.contains(' ') {
        view! { <span class="show-binding">{key}</span> }.into_any()
    } else {
        view! { <kbd class="show-key">{key}</kbd> }.into_any()
    }
}

/// The Run view.
#[component]
pub fn RunView() -> impl IntoView {
    view! {
        <div class="show-run">
            <RunBar />
            <div class="show-run-body">
                <div class="show-run-left">
                    <NowNext />
                </div>
                <Bookmarks />
                <div class="show-run-right">
                    <ClocksCard />
                    <UpNowCard />
                </div>
            </div>
            <FoldedStrip />
            <p class="show-run-foot">
                "Run view: the same show with the tracks folded into one strip · a click on the strip arms that spot for the next Continue, it does not jump · Edit brings the tracks back"
            </p>
        </div>
    }
}

/// The Run bar.
#[component]
fn RunBar() -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let id = move || panel.show().map(|s| s.id).unwrap_or_default();
    let running = move || panel.run.with(|r| r.show.is_some());
    let locked = move || panel.show().is_some_and(|s| s.locked);
    let rehearsing = move || panel.run.with(|r| r.rehearsing);
    let monitor = move || panel.show().map(|s| s.next_monitor).unwrap_or_default();
    let pick = move |m: NextMonitor| move |_| panel.send(Command::NextMonitor(id(), m));
    let mon = move |m: NextMonitor, label: &'static str| {
        view! {
            <button type="button" class:on=move || monitor() == m
                aria-pressed=move || (monitor() == m).to_string()
                on:click=pick(m)>{label}</button>
        }
    };
    view! {
        <div class="show-bar show-run-bar">
            <ModeSwitch />
            {move || if running() {
                view! {
                    <button type="button" class="show-btn"
                        on:click=move |_| panel.send(Command::Stop)>"■ Stop show"</button>
                }.into_any()
            } else {
                view! {
                    <button type="button" class="show-btn"
                        on:click=move |_| panel.send(Command::Play(id()))>"▶ Play show"</button>
                }.into_any()
            }}
            <button type="button" class="show-btn show-lock" class:on=locked
                aria-pressed=move || locked().to_string()
                on:click=move |_| panel.send(Command::Lock(id(), !locked()))>
                {move || if locked() { "🔒 Edits locked" } else { "🔓 Lock edits" }}
            </button>
            <button type="button" class="show-btn"
                aria-pressed=move || rehearsing().to_string()
                on:click=move |_| panel.send(Command::Rehearse(!rehearsing()))>
                {move || if rehearsing() { "Rehearse on preview: on" } else { "Rehearse on preview: off" }}
            </button>
            <div class="show-seg" role="group" aria-label="Next monitor">
                <span class="show-seg-label">"Next monitor"</span>
                {mon(NextMonitor::Live, "Live")}
                {mon(NextMonitor::Still, "Still")}
                {mon(NextMonitor::Off, "Off")}
            </div>
            <span class="show-run-status" class:paused=move || panel.run.with(|r| matches!(r.phase, RunPhase::Paused { .. }))>
                {move || {
                    let now = panel.clock.get();
                    let Some(show) = panel.show() else { return String::new() };
                    panel.run.with(|run| {
                        let text = super::status(&show, run, now);
                        if matches!(run.phase, RunPhase::Paused { .. }) {
                            format!("❚❚ {text}")
                        } else {
                            text
                        }
                    })
                }}
            </span>
        </div>
    }
}

/// The NOW and NEXT cards, Continue, Back and Continue's bindings.
#[component]
fn NowNext() -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let cards = move || {
        let show = panel.show()?;
        let head = panel.head();
        let now = panel.clock.get();
        Some(
            panel
                .run
                .with(|run| (self::now(&show, run, head, now), next(&show, run, head))),
        )
    };
    let busy = move || panel.run.with(|r| r.phase == RunPhase::Transition);
    view! {
        {move || cards().map(|(n, x)| view! {
            <div class="show-card">
                <h6>"NOW"</h6>
                <div class="show-card-big">{n.title}</div>
                <div class="show-card-sub">{n.line}</div>
            </div>
            <div class="show-card">
                <h6>"NEXT ON CONTINUE"</h6>
                <div class="show-card-big">{x.title}</div>
                <div class="show-card-sub">{x.line}</div>
            </div>
        })}
        <button type="button" class="show-continue" class:busy=busy aria-disabled=move || busy().to_string()
            on:click=move |_| if !busy() { panel.send(Command::Continue) }>"Continue ▸"</button>
        <div class="show-run-back">
            <button type="button" class="show-btn" on:click=move |_| panel.send(Command::Back)>"⏮ Back"</button>
            {move || panel.run.with(|r| r.continue_keys.clone()).into_iter().map(binding).collect_view()}
        </div>
    }
}

/// The bookmark buttons.
#[component]
fn Bookmarks() -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let list = move || {
        let Some(show) = panel.show() else {
            return Vec::new();
        };
        let head = panel.head();
        let now = panel.clock.get();
        panel.run.with(|run| cards(&show, run, head, now))
    };
    view! {
        <div class="show-run-marks">
            <h6>"BOOKMARKS"<span>" · click or press the key to go there now"</span></h6>
            <div class="show-run-grid">
                {move || list().into_iter().map(|c| {
                    let id = c.id.clone();
                    view! {
                        <button type="button" class="show-mark-card" class:here=c.here
                            class:loop-start=c.loop_start class:past=c.past
                            on:click=move |_| panel.send(Command::GoTo(id.clone()))>
                            <span class="show-mark-n">{c.number}</span>
                            <b>{c.name}</b>
                            <span class="show-mark-scene">{c.scene}</span>
                            <span class="show-mark-foot">
                                <span>
                                    <span class=format!("show-tag show-tag-{}", c.tag.class())>{c.tag.label()}</span>
                                    " " {c.at}
                                </span>
                                {c.key.map(|k| view! { <kbd class="show-key">{k}</kbd> })}
                            </span>
                        </button>
                    }
                }).collect_view()}
            </div>
        </div>
    }
}

/// The CLOCKS card.
#[component]
fn ClocksCard() -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let read = move || {
        let show = panel.show()?;
        let head = panel.head();
        Some(panel.run.with(|run| clocks(&show, run, head)))
    };
    view! {
        <div class="show-card">
            <h6>"CLOCKS"</h6>
            {move || read().map(|c| view! {
                <div class="show-clocks">
                    <div><span>"Show time"</span><b>{c.show}</b></div>
                    <div><span>"Live"</span><b>{c.live}</b></div>
                    <div><span>"Left in this clip"</span><b>{c.left}</b></div>
                    <div><span>{c.plan_label}</span><b class:late=c.late>{c.against}</b></div>
                </div>
            })}
        </div>
    }
}

/// The UP RIGHT NOW card.
#[component]
fn UpNowCard() -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let rows = move || {
        let Some(show) = panel.show() else {
            return Vec::new();
        };
        let head = panel.head();
        panel.run.with(|run| up_now(&show, run, head))
    };
    view! {
        <div class="show-card">
            <h6>"UP RIGHT NOW"</h6>
            <ul class="show-up">
                {move || {
                    let rows = rows();
                    if rows.is_empty() {
                        view! { <li class="hint">"Nothing over program"</li> }.into_any()
                    } else {
                        rows.into_iter().map(|r| view! { <li>{r.name}<i>{r.note}</i></li> }).collect_view().into_any()
                    }
                }}
            </ul>
        </div>
    }
}

/// The tracks folded into one strip. A click while the show runs arms that spot.
#[component]
fn FoldedStrip() -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let layout = move || {
        let show = panel.show()?;
        let head = panel.head();
        let armed = panel.run.with(|r| r.armed_ms);
        Some(strip(&show, head, armed))
    };
    let arm = move |ev: leptos::ev::MouseEvent| {
        if !panel.run.with(|r| r.show.is_some()) {
            return;
        }
        let (Some(show), Some(el)) = (
            panel.show(),
            ev.current_target()
                .and_then(|t| leptos::wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(t).ok()),
        ) else {
            return;
        };
        let rect = el.get_bounding_client_rect();
        let f = (f64::from(ev.client_x()) - rect.left()) / rect.width().max(1.0);
        panel.send(Command::Arm(Some(strip_ms(&show, f))));
    };
    view! {
        <div class="show-strip" class:live=move || panel.run.with(|r| r.show.is_some())
            title="Click to arm that spot for the next Continue" on:click=arm>
            {move || layout().map(|s| view! {
                {s.loops.into_iter().map(|(l, w)| view! { <div class="show-strip-loop" style:left=pc(l) style:width=pc(w)></div> }).collect_view()}
                {s.segments.into_iter().map(|g| view! {
                    <div class=format!("show-strip-seg show-seg-{}", g.colour) style:left=pc(g.left) style:width=pc(g.width)>{g.label}</div>
                }).collect_view()}
                {s.marks.into_iter().map(|(x, pausing)| view! {
                    <div class="show-strip-mark" class:pausing=pausing style:left=pc(x)></div>
                }).collect_view()}
                {s.armed.map(|x| view! {
                    <button type="button" class="show-strip-armed" style:left=pc(x)
                        title="Armed for the next Continue; click to disarm"
                        on:click=move |ev| { ev.stop_propagation(); panel.send(Command::Arm(None)); }>
                        "armed"
                    </button>
                })}
                <div class="show-strip-head" style:left=pc(s.head)></div>
            })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{TrackKind, TrackView, demo};
    use super::*;

    fn clip(id: &str, start_ms: u32, len_ms: u32, label: &str, kind: ClipKind) -> ClipView {
        ClipView {
            id: id.to_owned(),
            start_ms,
            len_ms,
            label: label.to_owned(),
            detail: String::new(),
            kind,
            transition_in: None,
            timed: false,
            missing: false,
            fades: None,
        }
    }

    fn track(id: &str, kind: TrackKind, clips: Vec<ClipView>) -> TrackView {
        TrackView {
            id: id.to_owned(),
            kind,
            clips,
            muted: false,
            locked: false,
        }
    }

    /// The mockup's show with its overlays, sequence and audio, paused at Close-up for 2:14.
    fn mockup() -> (ShowView, model::RunView, f64) {
        let mut show = demo::friday();
        let scene = |s: &str| ClipKind::Scene(s.to_owned());
        for t in &mut show.tracks {
            if t.kind == TrackKind::Keyframes {
                for k in &mut t.clips {
                    if k.id == "k2" {
                        k.transition_in = Some("Move 0.8 s".to_owned());
                    }
                }
                let screen = ClipKind::Keyframe("screen".to_owned());
                t.clips.push(clip("k3", 510_000, 0, "Screen full", screen));
            }
        }
        let mut ticker = clip(
            "s1",
            240_000,
            360_000,
            "Ticker",
            ClipKind::Sequence("tick".to_owned()),
        );
        ticker.detail = "looping".to_owned();
        show.tracks.splice(
            0..0,
            [
                track(
                    "v4",
                    TrackKind::Overlay(4),
                    vec![clip("o3", 330_000, 60_000, "Title", scene("title"))],
                ),
                track(
                    "v3",
                    TrackKind::Overlay(3),
                    vec![clip(
                        "o2",
                        240_000,
                        144_000,
                        "Lower third · Host",
                        scene("lower"),
                    )],
                ),
                track(
                    "v2",
                    TrackKind::Overlay(2),
                    vec![clip("o1", 0, 660_000, "Logo bug", scene("logo"))],
                ),
            ],
        );
        show.tracks
            .push(track("s1", TrackKind::Sequences(1), vec![ticker]));
        show.tracks.push(track(
            "a1",
            TrackKind::Audio(1),
            vec![clip(
                "a1c",
                0,
                660_000,
                "Mic · Host",
                ClipKind::Audio("mic".to_owned()),
            )],
        ));
        let run = model::RunView {
            show: Some(show.id.clone()),
            phase: RunPhase::Paused {
                bookmark: "b3".to_owned(),
                since: 1000.0,
            },
            at_ms: 330_000,
            live_ms: Some(768_000),
            elapsed_ms: Some(1_724_000),
            continue_keys: vec!["Space".to_owned(), "PageDown".to_owned()],
            ..model::RunView::default()
        };
        (show, run, 135_000.0)
    }

    #[test]
    fn now_shows_program_before_the_pause_fires() {
        let (show, run, t) = mockup();
        assert_eq!(
            now(&show, &run, 330_000, t),
            Card {
                title: "Main · Wide".to_owned(),
                line: "Paused at bookmark Close-up for 2:14".to_owned(),
            }
        );
    }

    #[test]
    fn now_while_playing_says_what_is_left() {
        let (show, mut run, _) = mockup();
        run.phase = RunPhase::Playing;
        let card = now(&show, &run, 330_000, 0.0);
        assert_eq!(card.title, "Main · Close");
        assert_eq!(card.line, "Playing · 1:30 left in this clip");
    }

    #[test]
    fn next_names_the_keyframe_its_move_and_what_fires() {
        let (show, run, _) = mockup();
        assert_eq!(
            next(&show, &run, 330_000),
            Card {
                title: "Close".to_owned(),
                line: "Keyframe Close, with a Move · then Show Title".to_owned(),
            }
        );
    }

    #[test]
    fn next_at_a_cut_names_the_scene_and_its_transition() {
        let (show, mut run, _) = mockup();
        run.phase = RunPhase::Paused {
            bookmark: "b2".to_owned(),
            since: 0.0,
        };
        let card = next(&show, &run, 180_000);
        assert_eq!(card.title, "intro.mp4");
        assert_eq!(card.line, "Scene intro.mp4, with a Fade");
    }

    #[test]
    fn next_while_playing_goes_to_the_next_pause_and_armed_wins() {
        let (show, mut run, _) = mockup();
        run.phase = RunPhase::Playing;
        assert_eq!(next(&show, &run, 430_000).title, "Outro");
        run.armed_ms = Some(510_000);
        let card = next(&show, &run, 430_000);
        assert_eq!(card.title, "Segment · Demo · Screen full");
        assert_eq!(card.line, "From the armed spot at 8:30");
    }

    #[test]
    fn cards_tag_each_bookmark_as_the_mockup_does() {
        let (show, run, t) = mockup();
        let cards = cards(&show, &run, 330_000, t);
        let read: Vec<_> = cards
            .iter()
            .map(|c| {
                (
                    c.number,
                    c.name.as_str(),
                    c.scene.as_str(),
                    c.tag.label(),
                    c.at.as_str(),
                )
            })
            .collect();
        assert_eq!(
            read,
            vec![
                (1, "Waiting", "Starting soon", "⟲ LOOP".to_owned(), "0:00"),
                (
                    2,
                    "Intro",
                    "intro.mp4 · 1:00",
                    "❚❚ PAUSES".to_owned(),
                    "3:00"
                ),
                (
                    3,
                    "Close-up",
                    "Main · keyframe Close",
                    "❚❚ HERE · 2:14".to_owned(),
                    "5:30"
                ),
                (4, "Demo", "Segment · Demo", "❚❚ PAUSES".to_owned(), "7:00"),
                (
                    5,
                    "Screen",
                    "Segment · Demo · keyframe Screen full",
                    "PLAYS THROUGH".to_owned(),
                    "8:30"
                ),
                (6, "Outro", "Outro", "❚❚ PAUSES".to_owned(), "10:00"),
            ]
        );
        assert!(cards[2].here && !cards[2].past);
        assert!(cards[0].past && cards[0].loop_start && cards[1].past);
        assert!(!cards[3].past);
        assert_eq!(cards[2].key.as_deref(), Some("F2"));
    }

    #[test]
    fn clocks_read_as_the_mockup() {
        let (show, run, _) = mockup();
        let c = clocks(&show, &run, 330_000);
        assert_eq!(
            (c.show.as_str(), c.live.as_str(), c.left.as_str()),
            ("5:30", "12:48", "1:30")
        );
        assert_eq!(c.plan_label, "Against plan (32:00)");
        assert_eq!(c.against, "+2:14");
        assert!(c.late);
    }

    #[test]
    fn clocks_without_live_or_plan_show_dashes() {
        let show = demo::friday();
        let c = clocks(&show, &model::RunView::default(), 0);
        assert_eq!(
            (c.live.as_str(), c.left.as_str(), c.against.as_str()),
            ("—", "—", "—")
        );
    }

    #[test]
    fn up_now_lists_overlays_then_sequences_then_audio() {
        let (show, run, _) = mockup();
        let rows: Vec<_> = up_now(&show, &run, 330_000)
            .into_iter()
            .map(|r| format!("{} | {}", r.name, r.note))
            .collect();
        assert_eq!(
            rows,
            vec![
                "Logo bug | overlay V2",
                "Lower third · Host | overlay V3 · 0:54 left",
                "Ticker | sequence · looping",
                "Mic · Host | audio A1",
            ]
        );
    }

    #[test]
    fn up_now_holds_a_clip_starting_at_the_head_once_playing() {
        let (show, mut run, _) = mockup();
        run.phase = RunPhase::Playing;
        let names: Vec<_> = up_now(&show, &run, 330_000)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert!(names.contains(&"Title".to_owned()));
    }

    #[test]
    fn strip_places_clips_marks_loop_and_head() {
        let (show, _, _) = mockup();
        let s = strip(&show, 330_000, Some(165_000));
        let colours: Vec<_> = s.segments.iter().map(|g| g.colour).collect();
        assert_eq!(colours, vec![0, 1, 2, 3, 4]);
        assert!((s.segments[2].left - 36.364).abs() < 0.01);
        assert!((s.segments[2].width - 27.273).abs() < 0.01);
        assert_eq!(s.head, 50.0);
        assert_eq!(s.armed, Some(25.0));
        assert_eq!(s.marks[2], (50.0, true));
        assert!(!s.marks[4].1);
        assert!((s.loops[0].1 - 27.273).abs() < 0.01);
        assert_eq!(strip_ms(&show, 0.5), 330_000);
        assert_eq!(strip_ms(&show, 2.0), 660_000);
    }
}
