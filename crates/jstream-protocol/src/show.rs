//! Shows ([ADR-0210](../../../docs/adr/0210-shows-a-run-order-with-bookmarks.md), since
//! 0.177): a saved run order of scenes, laid out on tracks in show time, with bookmarks
//! the show pauses at or jumps to, and loops.
//!
//! A show is a collection object (`show.set`, `show.delete`, listed in `state.get`'s
//! `shows` and saved in the file). A collection holds any number of shows and runs one at
//! a time. Running it is studio state, like the program scene: `show.play`,
//! `show.continue` and the other `show.*` actions drive it, `studio.get` reports it in
//! `show`, and `show.changed` says what it did. None of them is an edit, so running a
//! show never fills an undo history and is never saved.
//!
//! The wire calls a sequence a timeline (`timeline.*`, ADR-0184), so a show is never
//! called one here.

use crate::command::Action;
use crate::finite::Finite32;
use crate::id::{SceneId, ShowId, SourceId, TimelineId, TransitionPresetId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The most shows one collection holds.
pub const MAX_SHOWS: usize = 64;
/// The longest show, in milliseconds of show time: 24 hours. Pauses take no show time.
pub const MAX_SHOW_MS: u32 = 86_400_000;
/// The most clips the program track holds.
pub const MAX_PROGRAM_CLIPS: usize = 1024;
/// The most overlay, sequence or audio tracks a show holds, each.
pub const MAX_SHOW_TRACKS: usize = 8;
/// The most clips, cues or markers one track holds.
pub const MAX_TRACK_ENTRIES: usize = 1024;
/// The most bookmarks one show holds.
pub const MAX_SHOW_BOOKMARKS: usize = 256;
/// The most loops one show holds.
pub const MAX_SHOW_LOOPS: usize = 64;
/// The longest show name, and the longest name of a track, bookmark or loop, in bytes.
pub const MAX_SHOW_NAME_LEN: usize = 256;
/// The longest note on a bookmark, in bytes.
pub const MAX_SHOW_NOTE_LEN: usize = 1024;
/// The longest fade an audio clip takes in or out, in milliseconds.
pub const MAX_SHOW_FADE_MS: u32 = 60_000;
/// The longest a pausing bookmark waits before it continues by itself, in milliseconds:
/// one hour.
pub const MAX_SHOW_WAIT_MS: u32 = 3_600_000;
/// The shortest loop, in milliseconds of show time, as a timeline's shortest repeat.
pub const MIN_SHOW_LOOP_MS: u32 = 100;
/// How long an image clip is when it is dropped on a show, unless the show sets its
/// own `image_ms`: ten seconds.
pub const DEFAULT_IMAGE_MS: u32 = 10_000;
/// How many bookmarks `show.go_to_number` reaches, by their order in the show.
pub const SHOW_NUMBERED_BOOKMARKS: u8 = 8;

/// A show: tracks of clips in show time, bookmarks and loops (ADR-0210 decisions 2 to
/// 4). Every element in it (a clip, a track, a marker, a cue, a bookmark, a loop) has an
/// ID unique within the show.
///
/// A clip naming a scene, source, timeline, keyframe or preset the collection doesn't
/// hold is kept, as a timeline's cue is, and does nothing when the show reaches it, so
/// deleting a scene never shifts a show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShowInfo {
    /// Its ID, unique among the collection's shows: 1 to 64 bytes of ASCII letters,
    /// digits, `_` and `-`.
    pub show_id: ShowId,
    /// Its display name, at most 256 bytes. Omitted means empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The program track (V1): scene clips end to end with no gaps, the first starting
    /// at 0. Each clip's start puts its scene on program. At most 1024; together at most
    /// 24 hours. Omitted for none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub program: Vec<ProgramClip>,
    /// Overlay tracks (V2 and up), bottom first: each draws its clips' scenes over
    /// program while they last. At most 8. Omitted for none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<OverlayTrack>,
    /// The keyframe track (K): markers that load a keyframe of the scene on program, in
    /// time order. At most 1024. Omitted for none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<KeyframeMarker>,
    /// Sequence tracks (S): cues that play a sequence or run one event. At most 8.
    /// Omitted for none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequences: Vec<SequenceTrack>,
    /// Audio tracks (A): clips that bring a source's fader up for their span. At most 8.
    /// Omitted for none, and with none a show changes nothing about audio.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<AudioTrack>,
    /// Its bookmarks, in time order, no two at the same time. At most 256. Omitted for
    /// none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<ShowBookmark>,
    /// Its loops, in time order, none overlapping another. At most 64. Omitted for none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<ShowLoop>,
    /// What happens when it reaches its end. Default `stop`.
    #[serde(default, skip_serializing_if = "ShowEnd::is_stop")]
    pub end: ShowEnd,
    /// The transition preset a jump arrives with, and a loop wraps with, unless the
    /// bookmark or loop names its own. Omitted for the collection's default preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<TransitionPresetId>,
    /// How long a dropped image's clip starts, in milliseconds, 1 to 24 hours. Default
    /// 10000; omitted when it is.
    #[serde(
        default = "default_image_ms",
        skip_serializing_if = "is_default_image_ms"
    )]
    pub image_ms: u32,
    /// What the Run view's Next monitor shows. Default `still`.
    #[serde(default, skip_serializing_if = "NextMonitor::is_still")]
    pub next_monitor: NextMonitor,
    /// How long the show is planned to run, in milliseconds of wall time, for the Run
    /// view's clock. Omitted for no plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_ms: Option<u32>,
}

fn default_image_ms() -> u32 {
    DEFAULT_IMAGE_MS
}

fn is_default_image_ms(ms: &u32) -> bool {
    *ms == DEFAULT_IMAGE_MS
}

/// A scene clip on the program track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProgramClip {
    /// Its ID, unique within the show.
    pub clip_id: String,
    /// The scene its start puts on program.
    pub scene_id: SceneId,
    /// How long it lasts, in milliseconds, at least 1. A clip whose scene plays a video
    /// or audio file takes the file's length when it is dropped.
    pub length_ms: u32,
    /// The transition preset of the cut into it, which wins over the scenes' own in and
    /// out. Omitted for `Default`: the usual order (the next scene's in, the current
    /// scene's out, the collection's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<TransitionPresetId>,
}

/// An overlay track: scene clips drawn over program, with gaps allowed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OverlayTrack {
    /// Its ID, unique within the show.
    pub track_id: String,
    /// Its display name, at most 256 bytes. Omitted means empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Its clips, in time order, none overlapping another. At most 1024. Omitted for
    /// none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<OverlayClip>,
}

/// A clip on an overlay track: its scene is shown over program from its start to its
/// end, through the overlay's own show and hide transitions, across cuts and pauses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OverlayClip {
    /// Its ID, unique within the show.
    pub clip_id: String,
    /// The scene it shows.
    pub scene_id: SceneId,
    /// When it starts, in milliseconds of show time.
    pub start_ms: u32,
    /// How long it lasts, in milliseconds, at least 1.
    pub length_ms: u32,
    /// How long it fades in, in milliseconds, `0..=60000`. Default 0, which shows it
    /// through the overlay's own show transition.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_in_ms: u32,
    /// How long it fades out, as `fade_in_ms`, through the overlay's own hide transition
    /// at 0. The two fades together fit in `length_ms`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fade_out_ms: u32,
}

fn is_zero(ms: &u32) -> bool {
    *ms == 0
}

/// A marker on the keyframe track: it loads a keyframe of the scene on program, as a
/// Move, the way `scene.set_keyframe` does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KeyframeMarker {
    /// Its ID, unique within the show.
    pub marker_id: String,
    /// When it fires, in milliseconds of show time.
    pub at_ms: u32,
    /// The keyframe scene to load. Omitted for the scene's own values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyframe_id: Option<SceneId>,
    /// The preset whose timing the Move takes. Omitted for the default preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<TransitionPresetId>,
}

/// A sequence track: cues that play a sequence or run one event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SequenceTrack {
    /// Its ID, unique within the show.
    pub track_id: String,
    /// Its display name, at most 256 bytes. Omitted means empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Its cues, in time order. At most 1024. Omitted for none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cues: Vec<ShowCue>,
}

/// A cue on a sequence track. One that runs `timeline.play` is drawn as a clip as wide
/// as the sequence; any other is a one-off event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShowCue {
    /// Its ID, unique within the show.
    pub cue_id: String,
    /// When it fires, in milliseconds of show time.
    pub at_ms: u32,
    /// What it does: an action a timeline's cue may run ([`Action::cue_problem`]),
    /// except `scene.switch`, since the program track puts scenes on program. Read as
    /// plain JSON, as a timeline's cue is. [`ShowCue::action`] parses it.
    #[schemars(with = "Action")]
    pub action: serde_json::Value,
}

impl ShowCue {
    /// A cue `cue_id` at `at_ms` running `action`.
    pub fn new(cue_id: impl Into<String>, at_ms: u32, action: &Action) -> Self {
        ShowCue {
            cue_id: cue_id.into(),
            at_ms,
            // An action is plain data (IDs and strings), which always serializes.
            action: serde_json::to_value(action).expect("invariant: an Action serializes to JSON"),
        }
    }

    /// The cue's action, or `None` when it is one this version doesn't know.
    pub fn action(&self) -> Option<Action> {
        serde_json::from_value(self.action.clone()).ok()
    }

    /// The sequence it plays, if it is a `timeline.play` cue.
    pub fn plays(&self) -> Option<TimelineId> {
        match self.action() {
            Some(Action::TimelinePlay { timeline_id, .. }) => Some(timeline_id),
            _ => None,
        }
    }
}

/// An audio track: clips that each bring one source's fader up for their span.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioTrack {
    /// Its ID, unique within the show.
    pub track_id: String,
    /// Its display name, at most 256 bytes. Omitted means empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Its clips, in time order, none overlapping another. At most 1024. Omitted for
    /// none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<AudioClip>,
}

/// A clip on an audio track: its source's fader rises to `gain_db` over `fade_in_ms`
/// from its start, and falls back over `fade_out_ms` before its end, as a sequence lane
/// moves a fader, so the mixer shows it and a hand on the fader takes over.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioClip {
    /// Its ID, unique within the show.
    pub clip_id: String,
    /// The source whose fader it moves.
    pub source_id: SourceId,
    /// When it starts, in milliseconds of show time.
    pub start_ms: u32,
    /// How long it lasts, in milliseconds, at least 1.
    pub length_ms: u32,
    /// How long it fades in, in milliseconds, `0..=60000`. Default 0.
    #[serde(default)]
    pub fade_in_ms: u32,
    /// How long it fades out, in milliseconds, `0..=60000`. Default 0. The two fades
    /// together fit in `length_ms`.
    #[serde(default)]
    pub fade_out_ms: u32,
    /// The fader's level while it is up, in dB, -100 to +24. Default 0.
    #[serde(default)]
    pub gain_db: Finite32,
}

/// A bookmark: a named point in show time that the show can jump to (ADR-0210
/// decision 4). A pausing one also stops the show there until `show.continue`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShowBookmark {
    /// Its ID, unique within the show.
    pub bookmark_id: String,
    /// Its display name, at most 256 bytes, shown on its flag and in "Next". Omitted
    /// means empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Where it is, in milliseconds of show time.
    pub at_ms: u32,
    /// Whether the show pauses here until `show.continue`. Default `true`; omitted when
    /// `true`.
    #[serde(
        default = "crate::channel::yes",
        skip_serializing_if = "crate::channel::is_true"
    )]
    pub pausing: bool,
    /// The transition preset a jump here arrives with. Omitted for the show's
    /// `preset_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<TransitionPresetId>,
    /// How long a pausing bookmark waits before it continues by itself, in milliseconds,
    /// 1 to 3600000. Omitted to wait for `show.continue`. Only a pausing bookmark has
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continue_after_ms: Option<u32>,
    /// A note for the operator, at most 1024 bytes. Omitted means empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// A loop: a span of show time that the show plays again and again once it is inside
/// it, until a jump outside it or `show.continue` leaves (ADR-0210 decision 7). A
/// bookmark at `start_ms` is inside the loop; one at `end_ms` is outside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShowLoop {
    /// Its ID, unique within the show.
    pub loop_id: String,
    /// Its display name, at most 256 bytes. Omitted means empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Where it starts, in milliseconds of show time.
    pub start_ms: u32,
    /// Where it ends, at least 100 ms after `start_ms`.
    pub end_ms: u32,
    /// The transition preset it wraps with. Omitted for the show's `preset_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<TransitionPresetId>,
    /// How `show.continue` leaves it. Default `at_once`.
    #[serde(default, skip_serializing_if = "LoopLeave::is_at_once")]
    pub leave: LoopLeave,
}

/// How `show.continue` leaves a loop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LoopLeave {
    /// At once: the show plays on from the loop's end.
    #[default]
    AtOnce,
    /// At the end of this pass, instead of wrapping.
    FinishPass,
    /// A value a newer server sends that this version doesn't know. Never sent; the
    /// engine refuses it in an edit with `invalid_argument`.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

impl LoopLeave {
    fn is_at_once(&self) -> bool {
        *self == LoopLeave::AtOnce
    }
}

/// What a show does when it reaches its end (ADR-0210 decision 11). A show never ends
/// the stream.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ShowEnd {
    /// It stops, and the last scene stays on program.
    #[default]
    Stop,
    /// It goes back to its first bookmark, or to its start if it has none, and plays on.
    Loop,
    /// It goes to a bookmark of its own.
    GoTo {
        /// The bookmark.
        bookmark_id: String,
    },
}

impl ShowEnd {
    fn is_stop(&self) -> bool {
        *self == ShowEnd::Stop
    }
}

/// What the Run view's Next monitor shows (ADR-0210 decision 13). Program is never
/// slowed to draw Next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NextMonitor {
    /// The next scene, rendered live as a second picture.
    Live,
    /// One picture of the next scene, drawn once when Next changes, while the engine has
    /// time to spare.
    #[default]
    Still,
    /// Nothing: program alone.
    Off,
    /// A value a newer server sends that this version doesn't know. Never sent; the
    /// engine refuses it in an edit with `invalid_argument`.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

impl NextMonitor {
    fn is_still(&self) -> bool {
        *self == NextMonitor::Still
    }
}

impl ShowInfo {
    /// A show with `show_id`, no tracks, no bookmarks and every setting at its default.
    pub fn new(show_id: impl Into<ShowId>) -> Self {
        ShowInfo {
            show_id: show_id.into(),
            name: String::new(),
            program: Vec::new(),
            overlays: Vec::new(),
            keyframes: Vec::new(),
            sequences: Vec::new(),
            audio: Vec::new(),
            bookmarks: Vec::new(),
            loops: Vec::new(),
            end: ShowEnd::Stop,
            preset_id: None,
            image_ms: DEFAULT_IMAGE_MS,
            next_monitor: NextMonitor::Still,
            planned_ms: None,
        }
    }

    /// Its length in milliseconds of show time: the program clips' lengths added up,
    /// saturating.
    pub fn length_ms(&self) -> u32 {
        self.program
            .iter()
            .fold(0u32, |sum, clip| sum.saturating_add(clip.length_ms))
    }

    /// Where each program clip starts, in milliseconds of show time, in clip order.
    pub fn program_starts(&self) -> impl Iterator<Item = u32> + '_ {
        self.program.iter().scan(0u32, |start, clip| {
            let at = *start;
            *start = start.saturating_add(clip.length_ms);
            Some(at)
        })
    }

    /// The bookmark with `bookmark_id`, if the show holds it.
    pub fn bookmark(&self, bookmark_id: &str) -> Option<&ShowBookmark> {
        self.bookmarks.iter().find(|b| b.bookmark_id == bookmark_id)
    }

    /// Why this is not a valid show, as the field and the reason, or `None` if it is.
    /// What its clips name (scenes, sources, sequences, keyframes, presets) is not looked
    /// up: a show keeps a clip whose scene is gone.
    pub fn problem(&self) -> Option<(String, String)> {
        fn problem(
            field: impl Into<String>,
            reason: impl Into<String>,
        ) -> Option<(String, String)> {
            Some((field.into(), reason.into()))
        }
        if let Err(reason) = crate::check_channel_id(self.show_id.as_str()) {
            return problem("show_id", reason);
        }
        if self.name.len() > MAX_SHOW_NAME_LEN {
            return problem("name", "must be at most 256 bytes");
        }
        if !(1..=MAX_SHOW_MS).contains(&self.image_ms) {
            return problem("image_ms", "must be within 1..=86400000 (24 hours)");
        }
        if self
            .planned_ms
            .is_some_and(|ms| ms == 0 || ms > MAX_SHOW_MS)
        {
            return problem("planned_ms", "must be within 1..=86400000 (24 hours)");
        }
        if self.next_monitor == NextMonitor::Unknown {
            return problem("next_monitor", "must be `live`, `still` or `off`");
        }
        let mut ids = Ids::default();
        if let Some(p) = self.program_problem(&mut ids) {
            return Some(p);
        }
        if let Some(p) = self.overlay_problem(&mut ids) {
            return Some(p);
        }
        if let Some(p) = self.keyframe_problem(&mut ids) {
            return Some(p);
        }
        if let Some(p) = self.sequence_problem(&mut ids) {
            return Some(p);
        }
        if let Some(p) = self.audio_problem(&mut ids) {
            return Some(p);
        }
        if let Some(p) = self.bookmark_problem(&mut ids) {
            return Some(p);
        }
        if let Some(p) = self.loop_problem(&mut ids) {
            return Some(p);
        }
        if let ShowEnd::GoTo { bookmark_id } = &self.end
            && self.bookmark(bookmark_id).is_none()
        {
            return problem("end.bookmark_id", "must name one of the show's bookmarks");
        }
        None
    }

    fn program_problem(&self, ids: &mut Ids) -> Option<(String, String)> {
        if self.program.len() > MAX_PROGRAM_CLIPS {
            return Some((
                "program".into(),
                "a show holds at most 1024 program clips".into(),
            ));
        }
        let mut total = 0u64;
        for (n, clip) in self.program.iter().enumerate() {
            let at = format!("program.{n}");
            if let Some(p) = ids.take(&at, "clip_id", &clip.clip_id) {
                return Some(p);
            }
            if clip.length_ms == 0 {
                return Some((format!("{at}.length_ms"), "must be at least 1".into()));
            }
            total += u64::from(clip.length_ms);
            if total > u64::from(MAX_SHOW_MS) {
                return Some((
                    format!("{at}.length_ms"),
                    "the program clips together run at most 86400000 ms (24 hours)".into(),
                ));
            }
        }
        None
    }

    fn overlay_problem(&self, ids: &mut Ids) -> Option<(String, String)> {
        if self.overlays.len() > MAX_SHOW_TRACKS {
            return Some((
                "overlays".into(),
                "a show holds at most 8 overlay tracks".into(),
            ));
        }
        for (t, track) in self.overlays.iter().enumerate() {
            let at = format!("overlays.{t}");
            if let Some(p) = track_head(ids, &at, &track.track_id, &track.name, track.clips.len()) {
                return Some(p);
            }
            let spans = track.clips.iter().map(|c| (c.start_ms, c.length_ms));
            if let Some(p) = spans_problem(&format!("{at}.clips"), spans) {
                return Some(p);
            }
            for (n, clip) in track.clips.iter().enumerate() {
                let at = format!("{at}.clips.{n}");
                if let Some(p) = ids.take(&at, "clip_id", &clip.clip_id) {
                    return Some(p);
                }
                if let Some(p) =
                    fades_problem(&at, clip.fade_in_ms, clip.fade_out_ms, clip.length_ms)
                {
                    return Some(p);
                }
            }
        }
        None
    }

    fn keyframe_problem(&self, ids: &mut Ids) -> Option<(String, String)> {
        if self.keyframes.len() > MAX_TRACK_ENTRIES {
            return Some((
                "keyframes".into(),
                "a show holds at most 1024 keyframe markers".into(),
            ));
        }
        let times = self.keyframes.iter().map(|m| m.at_ms);
        if let Some(p) = times_problem("keyframes", times, false) {
            return Some(p);
        }
        for (n, marker) in self.keyframes.iter().enumerate() {
            if let Some(p) = ids.take(&format!("keyframes.{n}"), "marker_id", &marker.marker_id) {
                return Some(p);
            }
        }
        None
    }

    fn sequence_problem(&self, ids: &mut Ids) -> Option<(String, String)> {
        if self.sequences.len() > MAX_SHOW_TRACKS {
            return Some((
                "sequences".into(),
                "a show holds at most 8 sequence tracks".into(),
            ));
        }
        for (t, track) in self.sequences.iter().enumerate() {
            let at = format!("sequences.{t}");
            if let Some(p) = track_head(ids, &at, &track.track_id, &track.name, track.cues.len()) {
                return Some(p);
            }
            let times = track.cues.iter().map(|c| c.at_ms);
            if let Some(p) = times_problem(&format!("{at}.cues"), times, false) {
                return Some(p);
            }
            for (n, cue) in track.cues.iter().enumerate() {
                let at = format!("{at}.cues.{n}");
                if let Some(p) = ids.take(&at, "cue_id", &cue.cue_id) {
                    return Some(p);
                }
                let Some(action) = cue.action() else {
                    return Some((
                        format!("{at}.action"),
                        "not an action this version knows".into(),
                    ));
                };
                if let Some((field, reason)) = action.cue_problem() {
                    return Some((format!("{at}.action.{field}"), reason.into()));
                }
                if matches!(action, Action::SceneSwitch { .. }) {
                    return Some((
                        format!("{at}.action.type"),
                        "a show's program track puts scenes on program: place a clip there".into(),
                    ));
                }
            }
        }
        None
    }

    fn audio_problem(&self, ids: &mut Ids) -> Option<(String, String)> {
        if self.audio.len() > MAX_SHOW_TRACKS {
            return Some(("audio".into(), "a show holds at most 8 audio tracks".into()));
        }
        for (t, track) in self.audio.iter().enumerate() {
            let at = format!("audio.{t}");
            if let Some(p) = track_head(ids, &at, &track.track_id, &track.name, track.clips.len()) {
                return Some(p);
            }
            let spans = track.clips.iter().map(|c| (c.start_ms, c.length_ms));
            if let Some(p) = spans_problem(&format!("{at}.clips"), spans) {
                return Some(p);
            }
            for (n, clip) in track.clips.iter().enumerate() {
                let at = format!("{at}.clips.{n}");
                if let Some(p) = ids.take(&at, "clip_id", &clip.clip_id) {
                    return Some(p);
                }
                if let Some(p) =
                    fades_problem(&at, clip.fade_in_ms, clip.fade_out_ms, clip.length_ms)
                {
                    return Some(p);
                }
                if let Some(reason) = crate::audio::gain_db_problem(clip.gain_db) {
                    return Some((format!("{at}.gain_db"), reason.into()));
                }
            }
        }
        None
    }

    fn bookmark_problem(&self, ids: &mut Ids) -> Option<(String, String)> {
        if self.bookmarks.len() > MAX_SHOW_BOOKMARKS {
            return Some((
                "bookmarks".into(),
                "a show holds at most 256 bookmarks".into(),
            ));
        }
        let times = self.bookmarks.iter().map(|b| b.at_ms);
        if let Some(p) = times_problem("bookmarks", times, true) {
            return Some(p);
        }
        for (n, bookmark) in self.bookmarks.iter().enumerate() {
            let at = format!("bookmarks.{n}");
            if let Some(p) = ids.take(&at, "bookmark_id", &bookmark.bookmark_id) {
                return Some(p);
            }
            if bookmark.name.len() > MAX_SHOW_NAME_LEN {
                return Some((format!("{at}.name"), "must be at most 256 bytes".into()));
            }
            if bookmark.note.len() > MAX_SHOW_NOTE_LEN {
                return Some((format!("{at}.note"), "must be at most 1024 bytes".into()));
            }
            match bookmark.continue_after_ms {
                Some(_) if !bookmark.pausing => {
                    return Some((
                        format!("{at}.continue_after_ms"),
                        "only a pausing bookmark waits".into(),
                    ));
                }
                Some(ms) if ms == 0 || ms > MAX_SHOW_WAIT_MS => {
                    return Some((
                        format!("{at}.continue_after_ms"),
                        "must be within 1..=3600000 (one hour)".into(),
                    ));
                }
                _ => {}
            }
        }
        None
    }

    fn loop_problem(&self, ids: &mut Ids) -> Option<(String, String)> {
        if self.loops.len() > MAX_SHOW_LOOPS {
            return Some(("loops".into(), "a show holds at most 64 loops".into()));
        }
        for (n, show_loop) in self.loops.iter().enumerate() {
            let at = format!("loops.{n}");
            if let Some(p) = ids.take(&at, "loop_id", &show_loop.loop_id) {
                return Some(p);
            }
            if show_loop.name.len() > MAX_SHOW_NAME_LEN {
                return Some((format!("{at}.name"), "must be at most 256 bytes".into()));
            }
            if show_loop.leave == LoopLeave::Unknown {
                return Some((
                    format!("{at}.leave"),
                    "must be `at_once` or `finish_pass`".into(),
                ));
            }
            if show_loop.end_ms > MAX_SHOW_MS {
                return Some((
                    format!("{at}.end_ms"),
                    "must be at most 86400000 (24 hours)".into(),
                ));
            }
            if show_loop.end_ms < show_loop.start_ms.saturating_add(MIN_SHOW_LOOP_MS) {
                return Some((
                    format!("{at}.end_ms"),
                    "must be at least 100 ms after `start_ms`".into(),
                ));
            }
            if n > 0 && show_loop.start_ms < self.loops[n - 1].end_ms {
                return Some((
                    format!("{at}.start_ms"),
                    "loops are in time order and don't overlap: this one starts before the \
                     one before it ends"
                        .into(),
                ));
            }
        }
        None
    }
}

/// The IDs a show has used so far, so each is unique within it.
#[derive(Default)]
struct Ids(BTreeSet<String>);

impl Ids {
    /// Checks `id`'s form and that the show hasn't used it, then keeps it.
    fn take(&mut self, at: &str, field: &str, id: &str) -> Option<(String, String)> {
        if let Err(reason) = crate::check_channel_id(id) {
            return Some((format!("{at}.{field}"), reason.into()));
        }
        if !self.0.insert(id.to_owned()) {
            return Some((
                format!("{at}.{field}"),
                "another clip, track, marker, cue, bookmark or loop in the show has this ID".into(),
            ));
        }
        None
    }
}

/// A track's ID, name and size.
fn track_head(
    ids: &mut Ids,
    at: &str,
    track_id: &str,
    name: &str,
    entries: usize,
) -> Option<(String, String)> {
    if let Some(p) = ids.take(at, "track_id", track_id) {
        return Some(p);
    }
    if name.len() > MAX_SHOW_NAME_LEN {
        return Some((format!("{at}.name"), "must be at most 256 bytes".into()));
    }
    if entries > MAX_TRACK_ENTRIES {
        return Some((at.to_owned(), "a track holds at most 1024 entries".into()));
    }
    None
}

/// Fades of at most a minute each that together fit in the clip.
fn fades_problem(at: &str, fade_in: u32, fade_out: u32, length: u32) -> Option<(String, String)> {
    if fade_in > MAX_SHOW_FADE_MS {
        return Some((
            format!("{at}.fade_in_ms"),
            "must be within 0..=60000".into(),
        ));
    }
    if fade_out > MAX_SHOW_FADE_MS {
        return Some((
            format!("{at}.fade_out_ms"),
            "must be within 0..=60000".into(),
        ));
    }
    if u64::from(fade_in) + u64::from(fade_out) > u64::from(length) {
        return Some((
            format!("{at}.fade_out_ms"),
            "the fades in and out together fit in the clip's `length_ms`".into(),
        ));
    }
    None
}

/// Times in order, within the longest show, and with `strict` none twice.
fn times_problem(
    at: &str,
    times: impl Iterator<Item = u32>,
    strict: bool,
) -> Option<(String, String)> {
    let mut before: Option<u32> = None;
    for (n, ms) in times.enumerate() {
        if ms > MAX_SHOW_MS {
            return Some((
                format!("{at}.{n}.at_ms"),
                "must be at most 86400000 (24 hours)".into(),
            ));
        }
        if let Some(b) = before
            && (ms < b || (strict && ms == b))
        {
            let reason = if strict {
                "must be in time order, none at the same time as the one before it"
            } else {
                "must be in time order: earlier than the one before it"
            };
            return Some((format!("{at}.{n}.at_ms"), reason.into()));
        }
        before = Some(ms);
    }
    None
}

/// Clips `(start_ms, length_ms)` in time order, each at least 1 ms long, none
/// overlapping another, all within the longest show.
fn spans_problem(at: &str, spans: impl Iterator<Item = (u32, u32)>) -> Option<(String, String)> {
    let mut end_before = 0u64;
    for (n, (start, length)) in spans.enumerate() {
        if length == 0 {
            return Some((format!("{at}.{n}.length_ms"), "must be at least 1".into()));
        }
        let end = u64::from(start) + u64::from(length);
        if end > u64::from(MAX_SHOW_MS) {
            return Some((
                format!("{at}.{n}.length_ms"),
                "a clip ends at most 86400000 ms (24 hours) into the show".into(),
            ));
        }
        if u64::from(start) < end_before {
            return Some((
                format!("{at}.{n}.start_ms"),
                "clips on a track are in time order and don't overlap: this one starts \
                 before the one before it ends"
                    .into(),
            ));
        }
        end_before = end;
    }
    None
}

/// A running show, as `studio.get` reports it in `show` (ADR-0210 decision 2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunningShow {
    /// The show.
    pub show_id: ShowId,
    /// Where its play head is, in milliseconds of show time.
    pub at_ms: u32,
    /// Whether it plays, waits at a pausing bookmark, or was taken off script.
    pub state: ShowState,
    /// The pausing bookmark it waits at. Omitted unless `state` is `paused`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookmark_id: Option<String>,
    /// How long it has waited at `bookmark_id`, in milliseconds. Omitted unless `state`
    /// is `paused`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waited_ms: Option<u32>,
    /// The scene switched by hand that took it off script. Omitted unless `state` is
    /// `off_script`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off_script: Option<SceneId>,
    /// The loop its play head is in. Omitted outside a loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_id: Option<String>,
    /// Which pass of `loop_id` it is on: 0 the first. Omitted outside a loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass: Option<u32>,
    /// The spot armed for the next `show.continue` (`show.arm`), in milliseconds of show
    /// time. Omitted when none is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armed_ms: Option<u32>,
    /// Whether it rehearses on preview and leaves program alone. Omitted when `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rehearse: bool,
}

/// What a running show is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ShowState {
    /// Its play head moves.
    Playing,
    /// It waits at a pausing bookmark for `show.continue`. Program keeps running.
    Paused,
    /// A scene was switched by hand while it ran: it waits where it was, and
    /// `show.continue` goes on to the next clip.
    OffScript,
    /// A value a newer server sends that this version doesn't know.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// What a running show did, as `show.changed` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ShowChange {
    /// `show.play` started it.
    Started,
    /// It reached a pausing bookmark and waits.
    Paused,
    /// It plays on from a pause, by `show.continue` or a bookmark's own wait.
    Continued,
    /// It jumped: to a bookmark, by `show.seek`, or to an armed spot. What the show
    /// placed before the landing point is applied as state; one-shots are not replayed.
    Jumped,
    /// It wrapped from a loop's end to its start.
    Looped,
    /// A scene was switched by hand: it waits off script.
    OffScript,
    /// It reached its end and stopped there.
    Ended,
    /// `show.stop` stopped it, or another show's `show.play` replaced it, or the show
    /// was deleted.
    Stopped,
    /// A value a newer server sends that this version doesn't know.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

#[cfg(test)]
mod tests;
