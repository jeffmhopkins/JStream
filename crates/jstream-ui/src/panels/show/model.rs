//! What the Show panel draws: a show as the panel sees it, its tracks, clips, bookmarks and
//! loops in show time, and where the runner is. The Edit view, the Run view and the zoom bar
//! all read this one shape, so each is a function of it and tested natively.
//!
//! It is built from the engine's show (`show.*`) and the runner's state in `studio.get`.
//! Times are in ms of show time, which stops during a pause: a pause has no length, so a
//! pausing bookmark is a single point.
//!
//! Docs: crates/jstream-ui/README.md#design

/// A show as the panel draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ShowView {
    /// The show's ID.
    pub id: String,
    /// Its name, in the show picker.
    pub name: String,
    /// Its tracks, top to bottom: overlays (highest first), program, keyframes, sequences,
    /// then audio.
    pub tracks: Vec<TrackView>,
    /// Its bookmarks, in show time order.
    pub bookmarks: Vec<BookmarkView>,
    /// Its loops, in show time order. They never overlap.
    pub loops: Vec<LoopView>,
    /// The planned length, for the clocks' "against plan", if the show has one.
    pub planned_ms: Option<u32>,
    /// What the Next monitor shows.
    pub next_monitor: NextMonitor,
    /// Whether editing is locked.
    pub locked: bool,
}

impl ShowView {
    /// The show's length: the end of its last clip on any track.
    pub fn length_ms(&self) -> u32 {
        self.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(ClipView::end_ms)
            .max()
            .unwrap_or(0)
    }

    /// The program track's clips, in order.
    pub fn program(&self) -> &[ClipView] {
        self.tracks
            .iter()
            .find(|t| t.kind == TrackKind::Program)
            .map_or(&[], |t| &t.clips)
    }

    /// The program clip playing at `at_ms`: the last that starts at or before it.
    pub fn program_at(&self, at_ms: u32) -> Option<&ClipView> {
        self.program()
            .iter()
            .take_while(|c| c.start_ms <= at_ms)
            .last()
    }

    /// The loop `at_ms` is inside: from its start, up to but not including its end.
    pub fn loop_at(&self, at_ms: u32) -> Option<&LoopView> {
        self.loops
            .iter()
            .find(|l| l.start_ms <= at_ms && at_ms < l.end_ms)
    }

    /// The first pausing bookmark after `at_ms`.
    pub fn next_pause(&self, at_ms: u32) -> Option<&BookmarkView> {
        self.bookmarks.iter().find(|b| b.pausing && b.at_ms > at_ms)
    }
}

/// Which kind a track is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackKind {
    /// V1: scene clips end to end, no gaps.
    Program,
    /// V2 and up: scene clips over program, gaps allowed. Numbered from 2.
    Overlay(u8),
    /// K: keyframe markers.
    Keyframes,
    /// S1 and up: sequence clips and one-off events. Numbered from 1.
    Sequences(u8),
    /// A1 and up: audio clips. Numbered from 1.
    Audio(u8),
    /// The scene audio of video track V`n` (1 for program): drawn from its clips'
    /// scenes, read only, each clip linked to its scene clip (ADR-0216).
    Linked(u8),
}

impl TrackKind {
    /// The track's short tag at the head of its row: V1, V2, K, S1, A1.
    pub fn tag(self) -> String {
        match self {
            TrackKind::Program => "V1".to_owned(),
            TrackKind::Overlay(n) => format!("V{n}"),
            TrackKind::Keyframes => "K".to_owned(),
            TrackKind::Sequences(n) => format!("S{n}"),
            TrackKind::Audio(n) => format!("A{n}"),
            TrackKind::Linked(n) => format!("V{n}"),
        }
    }

    /// The track's name at the head of its row.
    pub fn title(self) -> &'static str {
        match self {
            TrackKind::Program => "Program",
            TrackKind::Overlay(_) => "Overlay",
            TrackKind::Keyframes => "Keyframes",
            TrackKind::Sequences(_) => "Sequences",
            TrackKind::Audio(_) => "Audio",
            TrackKind::Linked(_) => "Scene audio",
        }
    }

    /// Its class suffix in style.css (`show-track-<class>`).
    pub fn class(self) -> &'static str {
        match self {
            TrackKind::Program => "program",
            TrackKind::Overlay(_) => "overlay",
            TrackKind::Keyframes => "keyframes",
            TrackKind::Sequences(_) => "sequences",
            TrackKind::Audio(_) => "audio",
            TrackKind::Linked(_) => "linked",
        }
    }
}

/// One track: a row of clips.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackView {
    /// The track's ID within its show.
    pub id: String,
    /// What it holds.
    pub kind: TrackKind,
    /// The name its author gave it; empty for the kind's own ([`TrackKind::title`]).
    pub name: String,
    /// Its clips, in show time order.
    pub clips: Vec<ClipView>,
    /// Whether it is muted: its clips do nothing when the show plays.
    pub muted: bool,
    /// Whether it is locked against edits.
    pub locked: bool,
}

/// One clip on a track.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipView {
    /// The clip's ID within its show.
    pub id: String,
    /// Where it starts, in show time.
    pub start_ms: u32,
    /// How long it lasts; 0 for a marker (a keyframe or a one-off event).
    pub len_ms: u32,
    /// Its name on the clip: the scene's, the sequence's, the file's.
    pub label: String,
    /// Its second line: the length and what is in it.
    pub detail: String,
    /// What it is.
    pub kind: ClipKind,
    /// The transition on the cut into it, for a program clip after the first.
    pub transition_in: Option<String>,
    /// Whether it holds a video or audio file, so a jump into it seeks (the clock badge).
    pub timed: bool,
    /// Whether what it names is gone: drawn as "(missing)", never dropped.
    pub missing: bool,
    /// For an audio clip: its fade in and out, in ms.
    pub fades: Option<(u32, u32)>,
    /// For a sequence clip: the shape of what it plays, as points (fraction of the clip's
    /// width, value from 0 to 1) joined by straight lines and drawn across the clip.
    /// Empty draws no line.
    pub curve: Vec<(f32, f32)>,
}

impl ClipView {
    /// Where it ends, in show time.
    pub fn end_ms(&self) -> u32 {
        self.start_ms.saturating_add(self.len_ms)
    }
}

/// What a clip is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipKind {
    /// A scene, by ID: on program (V1) or over it (V2 and up).
    Scene(String),
    /// A keyframe of the scene on program, by ID.
    Keyframe(String),
    /// A sequence, by ID, playing for the clip's width.
    Sequence(String),
    /// A one-off event, described.
    Event(String),
    /// An audio source, by ID, up for the clip.
    Audio(String),
    /// A scene clip's own audio, by the scene's ID: its ID is the scene clip's.
    Linked(String),
}

/// A show bookmark: a named point the show can jump to.
#[derive(Clone, Debug, PartialEq)]
pub struct BookmarkView {
    /// The bookmark's ID within its show.
    pub id: String,
    /// Its name, on its flag.
    pub name: String,
    /// Where it is, in show time.
    pub at_ms: u32,
    /// Whether the show stops there until Continue: dashed line, pause symbol on the flag.
    pub pausing: bool,
    /// The key bound to "go to" it, if any, as it shows on a key cap: F2.
    pub key: Option<String>,
    /// What to say or do there, read from the Run view's notes like a teleprompter.
    pub note: String,
}

/// A loop: a region the play head goes round until the operator leaves.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopView {
    /// The loop's ID within its show.
    pub id: String,
    /// Its name, on its bar.
    pub name: String,
    /// Where it starts; a bookmark here is inside the loop.
    pub start_ms: u32,
    /// Where it ends; a bookmark here is outside the loop.
    pub end_ms: u32,
}

/// What the Next monitor beside program shows, per show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NextMonitor {
    /// The next scene, rendered live as a second picture.
    Live,
    /// One picture of it, drawn once when Next changes.
    #[default]
    Still,
    /// Nothing: program alone.
    Off,
}

/// Where the runner is, as `studio.get` and its events report it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunView {
    /// The running show's ID, if one runs.
    pub show: Option<String>,
    /// What it is doing.
    pub phase: RunPhase,
    /// The play head, in show time.
    pub at_ms: u32,
    /// When the UI heard `at_ms`, as `performance.now()` in ms, so the play head can run on
    /// between reports while playing.
    pub heard_at: f64,
    /// The spot armed for the next Continue, if any.
    pub armed_ms: Option<u32>,
    /// The loop it is going round, and which pass, counted from 1.
    pub looping: Option<(String, u32)>,
    /// Whether it runs on preview (Rehearse), leaving program alone.
    pub rehearsing: bool,
    /// The name of the scene a hand switch put on program, taking the show off script.
    pub off_script: Option<String>,
    /// How long the stream has been live, for the Run view's clocks, if it is.
    pub live_ms: Option<u32>,
    /// How long the show has run in wall time, pauses included, for "against plan".
    pub elapsed_ms: Option<u32>,
    /// The bindings of Continue (`show.continue`) as they show on key caps: "Space",
    /// "PageDown", "MIDI note 36 · ch 10".
    pub continue_keys: Vec<String>,
}

/// What the runner is doing.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum RunPhase {
    /// No show runs.
    #[default]
    Stopped,
    /// The play head moves.
    Playing,
    /// Stopped at a pausing bookmark, by ID, waiting for Continue; `since` is when the
    /// pause began, as `performance.now()` in ms, for the time waited.
    Paused {
        /// The bookmark it waits at.
        bookmark: String,
        /// When the wait began, as `performance.now()` in ms.
        since: f64,
    },
    /// A transition is running; Continue is ignored until it ends.
    Transition,
}

impl RunView {
    /// The play head at `now` (`performance.now()` in ms): run on from the last report
    /// while playing, held otherwise.
    pub fn head_at(&self, now: f64) -> u32 {
        match self.phase {
            RunPhase::Playing => {
                let ran = (now - self.heard_at).max(0.0);
                self.at_ms.saturating_add(ran as u32)
            }
            _ => self.at_ms,
        }
    }
}

#[cfg(test)]
pub(crate) mod demo {
    //! The mockup's show, "Friday show", for tests.

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
            curve: Vec::new(),
        }
    }

    fn mark(id: &str, name: &str, at_ms: u32, pausing: bool, key: &str) -> BookmarkView {
        BookmarkView {
            id: id.to_owned(),
            name: name.to_owned(),
            at_ms,
            pausing,
            key: Some(key.to_owned()),
            note: String::new(),
        }
    }

    fn track(id: &str, kind: TrackKind, clips: Vec<ClipView>) -> TrackView {
        TrackView {
            id: id.to_owned(),
            kind,
            name: String::new(),
            clips,
            muted: false,
            locked: false,
        }
    }

    fn detailed(mut c: ClipView, detail: &str) -> ClipView {
        c.detail = detail.to_owned();
        c
    }

    fn curved(mut c: ClipView, curve: &[(f32, f32)]) -> ClipView {
        c.curve = curve.to_vec();
        c
    }

    fn faded(mut c: ClipView, fade_in: u32, fade_out: u32, timed: bool) -> ClipView {
        c.fades = Some((fade_in, fade_out));
        c.timed = timed;
        c
    }

    /// The mockup's show: five program clips over 11 minutes, two overlay tracks,
    /// keyframes, two sequence tracks with one-off events, two audio tracks, six
    /// bookmarks and a loop.
    pub fn friday() -> ShowView {
        let scene = |s: &str| ClipKind::Scene(s.to_owned());
        let key = |s: &str| ClipKind::Keyframe(s.to_owned());
        let seq = |s: &str| ClipKind::Sequence(s.to_owned());
        let event = |s: &str| ClipKind::Event(s.to_owned());
        let audio = |s: &str| ClipKind::Audio(s.to_owned());
        let mut intro = clip("c2", 180_000, 60_000, "intro.mp4", scene("intro"));
        intro.timed = true;
        intro.detail = "1:00 · video length".to_owned();
        intro.transition_in = Some("Fade 0.5 s".to_owned());
        let mut main = clip("c3", 240_000, 180_000, "Main", scene("main"));
        main.detail = "3:00 · bookmarks at 1:30 and the end".to_owned();
        main.transition_in = Some("Move 0.8 s".to_owned());
        let mut demo = clip("c4", 420_000, 180_000, "Segment · Demo", scene("demo"));
        demo.detail = "3:00 · bookmark at the end".to_owned();
        demo.transition_in = Some("Cut".to_owned());
        let mut outro = clip("c5", 600_000, 60_000, "Outro", scene("outro"));
        outro.detail = "1:00".to_owned();
        outro.transition_in = Some("Fade 1 s".to_owned());
        let wave: Vec<(f32, f32)> = (0..=20u8)
            .map(|n| (f32::from(n) / 20.0, if n % 2 == 0 { 0.35 } else { 0.65 }))
            .collect();
        ShowView {
            id: "friday".to_owned(),
            name: "Friday show".to_owned(),
            tracks: vec![
                track(
                    "v3",
                    TrackKind::Overlay(3),
                    vec![
                        detailed(
                            clip("o1", 240_000, 84_000, "Lower third · Host", scene("lower")),
                            "1:24",
                        ),
                        detailed(
                            clip("o2", 456_000, 108_000, "▣ sponsor.png", scene("sponsor")),
                            "1:48",
                        ),
                    ],
                ),
                track(
                    "v2",
                    TrackKind::Overlay(2),
                    vec![detailed(
                        clip("o3", 240_000, 360_000, "Logo bug", scene("logo")),
                        "stays up through both bookmarks",
                    )],
                ),
                track(
                    "v1",
                    TrackKind::Program,
                    vec![
                        detailed(
                            clip("c1", 0, 180_000, "Starting soon", scene("soon")),
                            "3:00",
                        ),
                        intro,
                        main,
                        demo,
                        outro,
                    ],
                ),
                track(
                    "k",
                    TrackKind::Keyframes,
                    vec![
                        clip("k1", 240_000, 0, "Wide", key("wide")),
                        clip("k2", 330_000, 0, "Close", key("close")),
                        clip("k3", 378_000, 0, "Two-up", key("two-up")),
                        clip("k4", 510_000, 0, "Screen full", key("screen")),
                    ],
                ),
                track(
                    "s1",
                    TrackKind::Sequences(1),
                    vec![
                        curved(
                            clip("q1", 0, 175_000, "Countdown", seq("countdown")),
                            &[(0.0, 0.1), (1.0, 0.9)],
                        ),
                        curved(
                            clip("q2", 180_000, 54_000, "Lights up", seq("lights-up")),
                            &[(0.0, 0.1), (0.4, 0.15), (0.6, 0.75), (1.0, 0.8)],
                        ),
                        clip("e1", 330_000, 0, "Show Title", event("Show Title")),
                        curved(
                            clip("q3", 420_000, 54_000, "Demo sting", seq("sting")),
                            &[(0.0, 0.15), (0.3, 0.8), (0.6, 0.2), (1.0, 0.7)],
                        ),
                        clip("e2", 540_000, 0, "Hide Title", event("Hide Title")),
                        curved(
                            clip("q4", 600_000, 60_000, "Lights down", seq("lights-down")),
                            &[(0.0, 0.8), (0.5, 0.75), (0.6, 0.2), (1.0, 0.1)],
                        ),
                    ],
                ),
                track(
                    "s2",
                    TrackKind::Sequences(2),
                    vec![curved(
                        clip(
                            "q5",
                            240_000,
                            360_000,
                            "Ticker · loops, keeps running while paused",
                            seq("ticker"),
                        ),
                        &wave,
                    )],
                ),
                track(
                    "a1",
                    TrackKind::Audio(1),
                    vec![faded(
                        detailed(
                            clip("a1", 240_000, 360_000, "Mic · Host", audio("mic")),
                            "fade in 0.5 s · fade out 1 s",
                        ),
                        500,
                        1000,
                        false,
                    )],
                ),
                track(
                    "a2",
                    TrackKind::Audio(2),
                    vec![
                        faded(
                            detailed(
                                clip("a2", 0, 240_000, "♪ bed.mp3", audio("bed")),
                                "fade out 4 s",
                            ),
                            0,
                            4000,
                            true,
                        ),
                        faded(
                            detailed(
                                clip("a3", 564_000, 96_000, "♪ bed.mp3", audio("bed")),
                                "fade in 3 s",
                            ),
                            3000,
                            0,
                            true,
                        ),
                    ],
                ),
            ],
            bookmarks: vec![
                mark("b1", "Waiting", 0, false, "F1"),
                mark("b2", "Intro", 180_000, true, "F6"),
                mark("b3", "Close-up", 330_000, true, "F2"),
                mark("b4", "Demo", 420_000, true, "F3"),
                mark("b5", "Screen", 510_000, false, "F5"),
                mark("b6", "Outro", 600_000, true, "F4"),
            ],
            loops: vec![LoopView {
                id: "l1".to_owned(),
                name: "Waiting room".to_owned(),
                start_ms: 0,
                end_ms: 180_000,
            }],
            planned_ms: Some(1_920_000),
            next_monitor: NextMonitor::Still,
            locked: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_is_the_last_clip_end() {
        assert_eq!(demo::friday().length_ms(), 660_000);
    }

    #[test]
    fn program_at_finds_the_clip_playing() {
        let show = demo::friday();
        assert_eq!(show.program_at(0).map(|c| c.id.as_str()), Some("c1"));
        assert_eq!(show.program_at(239_999).map(|c| c.id.as_str()), Some("c2"));
        assert_eq!(show.program_at(240_000).map(|c| c.id.as_str()), Some("c3"));
    }

    #[test]
    fn a_loop_holds_its_start_but_not_its_end() {
        let show = demo::friday();
        assert!(show.loop_at(0).is_some());
        assert!(show.loop_at(179_999).is_some());
        assert!(show.loop_at(180_000).is_none());
    }

    #[test]
    fn next_pause_skips_non_pausing_bookmarks() {
        let show = demo::friday();
        assert_eq!(show.next_pause(0).map(|b| b.id.as_str()), Some("b2"));
        assert_eq!(show.next_pause(420_000).map(|b| b.id.as_str()), Some("b6"));
    }

    #[test]
    fn the_play_head_runs_on_only_while_playing() {
        let mut run = RunView {
            at_ms: 1000,
            heard_at: 500.0,
            phase: RunPhase::Playing,
            ..RunView::default()
        };
        assert_eq!(run.head_at(750.0), 1250);
        run.phase = RunPhase::Transition;
        assert_eq!(run.head_at(750.0), 1000);
    }
}
