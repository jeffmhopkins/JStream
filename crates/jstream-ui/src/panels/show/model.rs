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
    /// The scene a hand switch put on program, taking the show off script.
    pub off_script: Option<String>,
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
        }
    }

    fn mark(id: &str, name: &str, at_ms: u32, pausing: bool, key: &str) -> BookmarkView {
        BookmarkView {
            id: id.to_owned(),
            name: name.to_owned(),
            at_ms,
            pausing,
            key: Some(key.to_owned()),
        }
    }

    /// The mockup's show: five program clips over 11 minutes, six bookmarks and a loop.
    pub fn friday() -> ShowView {
        let scene = |s: &str| ClipKind::Scene(s.to_owned());
        let mut intro = clip("c2", 180_000, 60_000, "intro.mp4", scene("intro"));
        intro.timed = true;
        intro.transition_in = Some("Fade 0.5 s".to_owned());
        let mut main = clip("c3", 240_000, 180_000, "Main", scene("main"));
        main.transition_in = Some("Move 0.8 s".to_owned());
        let mut demo = clip("c4", 420_000, 180_000, "Segment · Demo", scene("demo"));
        demo.transition_in = Some("Cut".to_owned());
        let mut outro = clip("c5", 600_000, 60_000, "Outro", scene("outro"));
        outro.transition_in = Some("Fade 1 s".to_owned());
        ShowView {
            id: "friday".to_owned(),
            name: "Friday show".to_owned(),
            tracks: vec![
                TrackView {
                    id: "v1".to_owned(),
                    kind: TrackKind::Program,
                    clips: vec![
                        clip("c1", 0, 180_000, "Starting soon", scene("soon")),
                        intro,
                        main,
                        demo,
                        outro,
                    ],
                    muted: false,
                    locked: false,
                },
                TrackView {
                    id: "k".to_owned(),
                    kind: TrackKind::Keyframes,
                    clips: vec![
                        clip(
                            "k1",
                            240_000,
                            0,
                            "Wide",
                            ClipKind::Keyframe("wide".to_owned()),
                        ),
                        clip(
                            "k2",
                            330_000,
                            0,
                            "Close",
                            ClipKind::Keyframe("close".to_owned()),
                        ),
                    ],
                    muted: false,
                    locked: false,
                },
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
