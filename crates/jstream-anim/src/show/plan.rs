//! A show as the runner reads it: its tracks flattened into clips with show-time starts,
//! its bookmarks, its loops and what happens at its end.

use std::fmt::Debug;

use crate::CueAction;

/// The caller's types a show is built from: the engine uses its protocol IDs and actions.
/// One bundle, so the plan and the runner name one type parameter, not seven.
pub trait ShowKeys {
    /// A scene.
    type Scene: Clone + Debug + PartialEq;
    /// A sequence (a protocol timeline).
    type Timeline: Clone + Debug + PartialEq + Ord;
    /// A bookmark's ID.
    type Bookmark: Clone + Debug + PartialEq;
    /// A loop's ID.
    type Loop: Clone + Debug + PartialEq;
    /// An overlay track's ID.
    type Track: Clone + Debug + PartialEq + Ord;
    /// A transition preset.
    type Preset: Clone + Debug + PartialEq;
    /// An event's action. Its [`CueAction::state_key`] tells a state event from a
    /// one-shot; the key is rendered with `Debug` in [`super::Applied::states`].
    type Action: Clone + Debug + PartialEq + CueAction<Key: Debug>;
}

/// A clip on the program track (V1). Program clips sit end to end from show time 0, so
/// a clip's start is the sum of the lengths before it ([`Plan::program_start`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ProgramClip<K: ShowKeys> {
    /// The scene it puts on program.
    pub scene: K::Scene,
    /// Its length in milliseconds; at least 1.
    pub length_ms: u32,
    /// The transition for the cut into it; `None` lets the engine pick (the scene's own,
    /// then the default).
    pub transition: Option<K::Preset>,
    /// Whether its scene holds a video or audio file, so landing in its middle seeks.
    pub timed: bool,
}

/// A clip on an overlay track (V2 and up).
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayClip<K: ShowKeys> {
    /// The track it sits on. Clips on one track don't overlap.
    pub track: K::Track,
    /// The scene it draws over program.
    pub scene: K::Scene,
    /// Its start in show time, in milliseconds.
    pub start_ms: u32,
    /// Its length in milliseconds; at least 1.
    pub length_ms: u32,
    /// Fade in, in milliseconds.
    pub fade_in_ms: u32,
    /// Fade out, in milliseconds.
    pub fade_out_ms: u32,
    /// Whether its scene holds a video or audio file.
    pub timed: bool,
}

/// A keyframe marker (K track): loads a keyframe of the scene on program, as a Move. It
/// holds until the program clip it sits in ends or the next marker.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyframeMark<K: ShowKeys> {
    /// Its time in show time, in milliseconds.
    pub at_ms: u32,
    /// The keyframe scene it loads; `None` for the scene's own values (no keyframe).
    pub keyframe: Option<K::Scene>,
    /// The Move's preset; `None` for the engine's.
    pub preset: Option<K::Preset>,
}

/// A sequence clip (S tracks): plays a sequence for the clip's span, from its start.
#[derive(Clone, Debug, PartialEq)]
pub struct SequenceClip<K: ShowKeys> {
    /// The sequence.
    pub timeline: K::Timeline,
    /// Its start in show time, in milliseconds.
    pub start_ms: u32,
    /// Its length in milliseconds; at least 1. The sequence stops at its end.
    pub length_ms: u32,
}

/// A one-off event (S tracks).
#[derive(Clone, Debug, PartialEq)]
pub struct ShowEvent<K: ShowKeys> {
    /// Its time in show time, in milliseconds.
    pub at_ms: u32,
    /// What it does.
    pub action: K::Action,
}

/// A show bookmark: a named point the show can jump to, which may pause.
#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark<K: ShowKeys> {
    /// Its ID.
    pub id: K::Bookmark,
    /// Its time in show time, in milliseconds.
    pub at_ms: u32,
    /// Whether the show waits here for Continue.
    pub pausing: bool,
    /// The transition for a program cut made by jumping here; `None` for the show's
    /// default.
    pub arrive: Option<K::Preset>,
    /// A pausing bookmark continues by itself after this many milliseconds of waiting.
    pub limit_ms: Option<u32>,
}

/// How Continue leaves a loop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Leave {
    /// Jump to the loop's end at once.
    #[default]
    AtOnce,
    /// Finish this pass, then play on past the end.
    FinishPass,
}

/// A loop region `[from_ms, to_ms)`.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopRegion<K: ShowKeys> {
    /// Its ID.
    pub id: K::Loop,
    /// Its start, in show time.
    pub from_ms: u32,
    /// Its end, in show time; after `from_ms`.
    pub to_ms: u32,
    /// How Continue leaves it.
    pub leave: Leave,
    /// The transition for a program cut made by wrapping; `None` for the show's default.
    pub wrap: Option<K::Preset>,
}

/// What a show does when it reaches its end. It never ends the stream.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum EndOfShow<K: ShowKeys> {
    /// Stop, leaving the last scene on program.
    #[default]
    Stop,
    /// Jump to the first bookmark, or the start if it has none.
    Loop,
    /// Jump to this bookmark.
    GoTo(K::Bookmark),
}

/// A show, as the runner reads it. Every list but `program` is in time order (ties keep
/// their order); [`Plan::check`] says whether it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan<K: ShowKeys> {
    /// The program track, end to end from 0.
    pub program: Vec<ProgramClip<K>>,
    /// The overlay clips of every overlay track, by start.
    pub overlays: Vec<OverlayClip<K>>,
    /// The keyframe markers, by time.
    pub keyframes: Vec<KeyframeMark<K>>,
    /// The sequence clips, by start.
    pub sequences: Vec<SequenceClip<K>>,
    /// The one-off events, by time.
    pub events: Vec<ShowEvent<K>>,
    /// The bookmarks, by time. Their order is "bookmark 1..8".
    pub bookmarks: Vec<Bookmark<K>>,
    /// The loops, by start; they don't overlap.
    pub loops: Vec<LoopRegion<K>>,
    /// What happens at the end.
    pub end: EndOfShow<K>,
    /// The transition for a landing or a wrap that names none.
    pub default_transition: Option<K::Preset>,
}

/// Why a plan is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidPlan {
    /// A clip at this index of its list has length 0.
    EmptyClip(usize),
    /// A list isn't in time order at this index.
    Unsorted(usize),
    /// Two clips on one overlay track overlap; the later one's index.
    OverlayOverlap(usize),
    /// The loop at this index is empty or overlaps the one before it.
    Loop(usize),
    /// The end names a bookmark the show doesn't have.
    EndBookmark,
}

impl<K: ShowKeys> Plan<K> {
    /// Where program clip `clip` starts, in show time. Past the last clip, the end of the
    /// program track.
    pub fn program_start(&self, clip: usize) -> u32 {
        self.program
            .iter()
            .take(clip)
            .fold(0u32, |sum, c| sum.saturating_add(c.length_ms))
    }

    /// Where the program track ends, in show time.
    pub fn program_end(&self) -> u32 {
        self.program_start(self.program.len())
    }

    /// The show's length: the latest end or time of anything on it.
    pub fn length_ms(&self) -> u32 {
        let overlays = self
            .overlays
            .iter()
            .map(|c| c.start_ms.saturating_add(c.length_ms));
        let sequences = self
            .sequences
            .iter()
            .map(|c| c.start_ms.saturating_add(c.length_ms));
        let keyframes = self.keyframes.iter().map(|m| m.at_ms);
        let events = self.events.iter().map(|e| e.at_ms);
        let bookmarks = self.bookmarks.iter().map(|b| b.at_ms);
        let loops = self.loops.iter().map(|l| l.to_ms);
        overlays
            .chain(sequences)
            .chain(keyframes)
            .chain(events)
            .chain(bookmarks)
            .chain(loops)
            .fold(self.program_end(), u32::max)
    }

    /// The first problem with it, or `Ok`.
    pub fn check(&self) -> Result<(), InvalidPlan> {
        let checks = [
            self.program.iter().position(|c| c.length_ms == 0),
            self.overlays.iter().position(|c| c.length_ms == 0),
            self.sequences.iter().position(|c| c.length_ms == 0),
        ];
        if let Some(i) = checks.into_iter().flatten().next() {
            return Err(InvalidPlan::EmptyClip(i));
        }
        let unsorted = |times: &[u32]| times.windows(2).position(|w| w[1] < w[0]).map(|i| i + 1);
        let lists: [Vec<u32>; 5] = [
            self.overlays.iter().map(|c| c.start_ms).collect(),
            self.keyframes.iter().map(|m| m.at_ms).collect(),
            self.sequences.iter().map(|c| c.start_ms).collect(),
            self.events.iter().map(|e| e.at_ms).collect(),
            self.bookmarks.iter().map(|b| b.at_ms).collect(),
        ];
        if let Some(i) = lists.iter().find_map(|l| unsorted(l)) {
            return Err(InvalidPlan::Unsorted(i));
        }
        for (j, later) in self.overlays.iter().enumerate() {
            let overlaps = self.overlays[..j].iter().any(|earlier| {
                earlier.track == later.track
                    && earlier.start_ms.saturating_add(earlier.length_ms) > later.start_ms
            });
            if overlaps {
                return Err(InvalidPlan::OverlayOverlap(j));
            }
        }
        let mut previous_end = None;
        for (i, region) in self.loops.iter().enumerate() {
            let overlaps = previous_end.is_some_and(|end| region.from_ms < end);
            if region.from_ms >= region.to_ms || overlaps {
                return Err(InvalidPlan::Loop(i));
            }
            previous_end = Some(region.to_ms);
        }
        if let EndOfShow::GoTo(id) = &self.end
            && self.bookmark(id).is_none()
        {
            return Err(InvalidPlan::EndBookmark);
        }
        Ok(())
    }

    /// The loop that `at_ms` is inside (`from_ms <= at_ms < to_ms`), by index.
    pub fn loop_at(&self, at_ms: u32) -> Option<usize> {
        self.loops
            .iter()
            .position(|l| l.from_ms <= at_ms && at_ms < l.to_ms)
    }

    /// The index of the bookmark with this ID.
    pub fn bookmark(&self, id: &K::Bookmark) -> Option<usize> {
        self.bookmarks.iter().position(|b| &b.id == id)
    }
}
