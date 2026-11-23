//! Timelines on the wire ([ADR-0099](../../../docs/adr/0099-native-motion-modifiers-and-timelines.md)
//! decision 4, [ADR-0115](../../../docs/adr/0115-timelines-on-the-wire.md)): timed cues a
//! scene holds, each running a protocol action at a time from the timeline's start.
//!
//! A scene holds its timelines in `SceneInfo.timelines`, which `timeline.set` and
//! `timeline.delete` edit. What is playing is studio state, like the program scene:
//! `timeline.play`, `timeline.stop` and `timeline.seek` are actions, and `studio.get`
//! reports the timelines playing. Each cue's action is one of a closed set
//! ([`Action::cue_problem`]), so playing a timeline never makes an edit.

use crate::command::Action;
use crate::id::{LabelId, SceneId, TimelineId};
use crate::{Finite32, check_channel_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most timelines one scene holds (ADR-0099 decision 5).
pub const MAX_TIMELINES: usize = 32;
/// The most cues one timeline holds (ADR-0099 decision 5).
pub const MAX_CUES: usize = 256;
/// The longest a timeline runs, in milliseconds: one hour (ADR-0099 decision 5).
pub const MAX_TIMELINE_MS: u32 = 3_600_000;

/// The shortest a repeat may be, in milliseconds: a `loop` timeline's `duration_ms`,
/// or a `range`'s `to_ms - from_ms`. It keeps `looped` events and the cues a repeat
/// runs well below once a frame.
pub const MIN_LOOP_MS: u32 = 100;
/// The longest timeline ID, in bytes.
pub const MAX_TIMELINE_ID_LEN: usize = 64;
/// The longest timeline name, in bytes.
pub const MAX_TIMELINE_NAME_LEN: usize = 256;

/// The most lanes one timeline holds (ADR-0184).
pub const MAX_LANES: usize = 32;

/// The most points one lane holds (ADR-0184).
pub const MAX_LANE_POINTS: usize = 256;

/// The most triggers in a timeline's `starts`, and in its `stops` (ADR-0184).
pub const MAX_TRIGGERS: usize = 16;

/// The longest wait before a play, in milliseconds: one minute (ADR-0184). It bounds a
/// trigger's `delay_ms` and `timeline.play`'s.
pub const MAX_TRIGGER_DELAY_MS: u32 = 60_000;

/// One timeline: cues at times from its start, a length, and how it repeats.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineInfo {
    /// Its ID, chosen by the client: 1 to 64 bytes of ASCII letters, digits, `_` and
    /// `-`, unique across the collection, so an action can name it alone.
    pub timeline_id: TimelineId,
    /// Its display name. Omitted means empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// How long one pass runs, in milliseconds: at most one hour. A timeline that
    /// repeats needs a length above 0.
    pub duration_ms: u32,
    /// How it repeats. Omitted means `once`.
    #[serde(rename = "loop", default)]
    pub looping: TimelineLoop,
    /// Its cues, in time order: each cue's `at_ms` is at least the one before it's, and
    /// cues at the same time fire in list order. At most 256.
    #[serde(default)]
    pub cues: Vec<TimelineCue>,
    /// The labels it carries (ADR-0133, since 0.100), ordered by ID, each at most once.
    /// Every one must exist. Omitted for none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<LabelId>,
    /// Its lanes (ADR-0184, since 0.170): each moves one channel over time while the
    /// timeline plays, as one more input of that channel. At most 32. Omitted for none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lanes: Vec<TimelineLane>,
    /// What plays it (ADR-0184, since 0.170): each trigger, when it fires, plays it from
    /// its start after the trigger's `delay_ms`. At most 16. Omitted for none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub starts: Vec<TimelineTrigger>,
    /// What stops it (ADR-0184, since 0.170), the same way. At most 16. Omitted for
    /// none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stops: Vec<TimelineTrigger>,
}

/// One lane: a channel moved through points while its timeline plays (ADR-0184).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineLane {
    /// What it moves.
    pub target: LaneTarget,
    /// Its points, in time order: each `at_ms` at least the one before it's and at most
    /// the timeline's `duration_ms`. A lane with no point at 0 starts from the channel's
    /// value when the timeline starts, and eases from there to its first point. At most
    /// 256.
    #[serde(default)]
    pub points: Vec<LanePoint>,
    /// Whether it plays. Omitted means on; a lane that is off does nothing.
    #[serde(
        default = "crate::channel::yes",
        skip_serializing_if = "crate::channel::is_true"
    )]
    pub on: bool,
}

/// What a lane moves: a channel by its ID. A channel that doesn't exist is taken, and the
/// lane drives nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LaneTarget {
    /// The channel.
    pub channel: String,
}

/// One point of a lane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LanePoint {
    /// When the lane reaches it, in milliseconds from the timeline's start.
    pub at_ms: u32,
    /// The channel's value there, 0 to 1.
    pub value: Finite32,
    /// How the lane moves from the point before (or the starting value) to this one.
    /// Omitted means `linear`.
    #[serde(default, skip_serializing_if = "LaneCurve::is_linear")]
    pub curve: LaneCurve,
}

/// How a lane moves into a point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LaneCurve {
    /// A straight line.
    #[default]
    Linear,
    /// Slow at first.
    EaseIn,
    /// Slow at the end.
    EaseOut,
    /// Slow at both ends.
    EaseInOut,
    /// Stays at the value before, then jumps to this point's at its time: a switch.
    Hold,
    /// A value a newer server sends that this version doesn't know.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

impl LaneCurve {
    fn is_linear(&self) -> bool {
        *self == LaneCurve::Linear
    }

    /// Where it is at `t`, 0 to 1 of the way, as 0 to 1 of the change.
    pub fn at(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            LaneCurve::Linear | LaneCurve::Unknown => t,
            LaneCurve::EaseIn => t * t,
            LaneCurve::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            LaneCurve::EaseInOut => t * t * (3.0 - 2.0 * t),
            LaneCurve::Hold => {
                if t >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

impl TimelineLane {
    /// Its value at `at_ms` into a pass, starting from `from` where it has no point at
    /// 0: the channel's value when the timeline started. Holds its last point's value
    /// after it, and `from` if it has no points.
    pub fn value_at(&self, at_ms: f64, from: f32) -> f32 {
        let mut before = (0.0_f64, from);
        for point in &self.points {
            let at = f64::from(point.at_ms);
            let value = point.value.get();
            if at_ms < at {
                let span = at - before.0;
                let t = if span <= 0.0 {
                    1.0
                } else {
                    ((at_ms - before.0) / span) as f32
                };
                return before.1 + (value - before.1) * point.curve.at(t);
            }
            before = (at, value);
        }
        before.1
    }
}

/// What fires a timeline's `starts` or `stops` entry (ADR-0184), tagged by `type`. Each
/// fires on a change only, never when a collection opens, and waits `delay_ms` (at most
/// 60000) first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TimelineTrigger {
    /// A channel crossing `level` (default 0.5) or switching on or off.
    Channel {
        /// The channel. One that doesn't exist never fires.
        channel_id: String,
        /// Which change.
        edge: ChannelEdge,
        /// The level `rises` and `falls` cross, 0 to 1. Omitted means 0.5.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<Finite32>,
        /// How long to wait first, in milliseconds.
        #[serde(default, skip_serializing_if = "is_zero")]
        delay_ms: u32,
    },
    /// A scene going on or off program, by any route.
    Scene {
        /// The scene.
        scene_id: SceneId,
        /// Which change.
        edge: SceneEdge,
        /// How long to wait first, in milliseconds.
        #[serde(default, skip_serializing_if = "is_zero")]
        delay_ms: u32,
    },
    /// A scene starting or ceasing to draw through a keyframe: a keyframe scene of it
    /// going on program, or `scene.set_keyframe` placing one.
    Keyframe {
        /// The scene whose keyframe it is.
        scene_id: SceneId,
        /// The keyframe. Omitted means any of its keyframes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keyframe_id: Option<SceneId>,
        /// Which change.
        edge: KeyframeEdge,
        /// How long to wait first, in milliseconds.
        #[serde(default, skip_serializing_if = "is_zero")]
        delay_ms: u32,
    },
    /// Another timeline starting, ending or looping.
    Timeline {
        /// The other timeline.
        timeline_id: TimelineId,
        /// Which change.
        edge: TimelineEdge,
        /// How long to wait first, in milliseconds.
        #[serde(default, skip_serializing_if = "is_zero")]
        delay_ms: u32,
    },
    /// A trigger a newer server sends that this version doesn't know.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Which change of a channel fires a trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelEdge {
    /// Its value crosses the level upward.
    Rises,
    /// Its value crosses the level downward.
    Falls,
    /// It switches on.
    On,
    /// It switches off.
    Off,
}

/// Which change of a scene fires a trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SceneEdge {
    /// It goes on program.
    Shown,
    /// Another scene replaces it on program.
    Hidden,
}

/// Which change of a keyframe fires a trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyframeEdge {
    /// The scene starts drawing through it.
    Loads,
    /// The scene stops drawing through it.
    Leaves,
}

/// Which change of another timeline fires a trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimelineEdge {
    /// It starts, or starts again, from 0 or by a seek.
    Starts,
    /// A `once` timeline reaches its end.
    Ends,
    /// It starts another pass.
    Loops,
}

impl TimelineTrigger {
    /// Its wait, in milliseconds; 0 for an unknown trigger.
    pub fn delay_ms(&self) -> u32 {
        match self {
            TimelineTrigger::Channel { delay_ms, .. }
            | TimelineTrigger::Scene { delay_ms, .. }
            | TimelineTrigger::Keyframe { delay_ms, .. }
            | TimelineTrigger::Timeline { delay_ms, .. } => *delay_ms,
            TimelineTrigger::Unknown => 0,
        }
    }

    /// The first problem with it, as the field within it and why.
    pub fn problem(&self) -> Option<(&'static str, &'static str)> {
        if self.delay_ms() > MAX_TRIGGER_DELAY_MS {
            return Some(("delay_ms", "must be at most 60000 (one minute)"));
        }
        match self {
            TimelineTrigger::Channel {
                channel_id, level, ..
            } => {
                if let Err(reason) = check_channel_id(channel_id) {
                    return Some(("channel_id", reason));
                }
                if level.is_some_and(|l| !(0.0..=1.0).contains(&l.get())) {
                    return Some(("level", "must be 0 to 1"));
                }
                None
            }
            TimelineTrigger::Timeline { timeline_id, .. } => {
                check_timeline_id(timeline_id.as_str())
                    .err()
                    .map(|reason| ("timeline_id", reason))
            }
            TimelineTrigger::Scene { .. } | TimelineTrigger::Keyframe { .. } => None,
            TimelineTrigger::Unknown => Some(("type", "is not a trigger this version knows")),
        }
    }
}

/// How a timeline repeats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimelineLoop {
    /// It plays once, and ends at its `duration_ms`. A cue at `duration_ms` fires.
    #[default]
    Once,
    /// It plays from 0 to `duration_ms` again and again, until stopped. A cue at
    /// `duration_ms` would never fire, so it is refused: put it at 0. `duration_ms` is at
    /// least 100 ([`MIN_LOOP_MS`]).
    Loop,
    /// It plays from 0, and each time it reaches `to_ms` jumps back to `from_ms`: an
    /// intro, then a loop. `from_ms < to_ms <= duration_ms`, `to_ms - from_ms` is at
    /// least 100 ([`MIN_LOOP_MS`]), and a cue at or after `to_ms` is refused.
    Range {
        /// Where each repeat starts, in milliseconds.
        from_ms: u32,
        /// Where each pass jumps back, in milliseconds.
        to_ms: u32,
    },
}

/// One timed action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineCue {
    /// When it fires, in milliseconds from the timeline's start.
    pub at_ms: u32,
    /// What it does: one of the actions [`Action::cue_problem`] allows. Read as plain
    /// JSON, as a hotkey's action is, so a client older than an action a later cue may
    /// run still reads the collection. [`TimelineCue::action`] parses it.
    #[schemars(with = "Action")]
    pub action: serde_json::Value,
}

impl TimelineCue {
    /// A cue at `at_ms` running `action`.
    pub fn new(at_ms: u32, action: &Action) -> Self {
        TimelineCue {
            at_ms,
            // An action is plain data (IDs and strings), which always serializes.
            action: serde_json::to_value(action).expect("invariant: an Action serializes to JSON"),
        }
    }

    /// The cue's action, or `None` when it is one this version doesn't know.
    pub fn action(&self) -> Option<Action> {
        serde_json::from_value(self.action.clone()).ok()
    }
}

/// A timeline that is playing, as `studio.get` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlayingTimeline {
    /// The timeline.
    pub timeline_id: TimelineId,
    /// The scene that holds it.
    pub scene_id: SceneId,
    /// Where it is in its pass, in milliseconds from its start.
    pub position_ms: u32,
    /// Which pass it is on: 0 the first. A `once` timeline has only pass 0.
    pub pass: u64,
}

/// A timeline waiting to play, as `studio.get` reports it (ADR-0184): a `timeline.play`
/// with a `delay_ms`, or a trigger with one, not yet due.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WaitingTimeline {
    /// The timeline.
    pub timeline_id: TimelineId,
    /// The scene that holds it.
    pub scene_id: SceneId,
    /// How long until it plays, in milliseconds.
    pub due_in_ms: u32,
}

/// Why a timeline's playing changed, in `timeline.changed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TimelineChange {
    /// `timeline.play`, a `timeline.play` cue, or a hotkey bound to one started it from
    /// 0, or started it again from 0.
    Started,
    /// `timeline.seek` started it from `position_ms`.
    Sought,
    /// It reached the end of a pass and started the next one.
    Looped,
    /// A `once` timeline reached its `duration_ms`, after its last cue fired.
    Ended,
    /// `timeline.stop`, or a `timeline.stop` cue, stopped it.
    Stopped,
    /// A batch, undo or redo deleted it, or the scene holding it, while it played.
    Removed,
    /// A value a newer server sends that this version doesn't know.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// Checks a timeline ID: 1 to [`MAX_TIMELINE_ID_LEN`] bytes of ASCII letters, digits,
/// `_` and `-`. Returns what it must be if it isn't.
pub fn check_timeline_id(id: &str) -> Result<(), &'static str> {
    let well_formed = !id.is_empty()
        && id.len() <= MAX_TIMELINE_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if well_formed {
        Ok(())
    } else {
        Err("must be 1 to 64 bytes of ASCII letters, digits, `_` and `-`")
    }
}

impl TimelineInfo {
    /// The first problem with it, as the field within the timeline and why, or `None`
    /// when `timeline.set` can store it: its ID, name, length and loop, its cue count,
    /// each cue in time order and reachable (a cue a repeat would never reach is
    /// refused), and each cue's action allowed in a cue ([`Action::cue_problem`]).
    /// Whether the objects a cue names exist is the engine's check.
    pub fn problem(&self) -> Option<(String, String)> {
        let problem = |field: &str, reason: &str| Some((field.to_owned(), reason.to_owned()));
        if let Err(reason) = check_timeline_id(self.timeline_id.as_str()) {
            return problem("timeline_id", reason);
        }
        if self.name.len() > MAX_TIMELINE_NAME_LEN {
            return problem("name", "must be at most 256 bytes");
        }
        if self.duration_ms > MAX_TIMELINE_MS {
            return problem("duration_ms", "must be at most 3600000 (one hour)");
        }
        let end = match self.looping {
            TimelineLoop::Once => None,
            TimelineLoop::Loop => {
                if self.duration_ms < MIN_LOOP_MS {
                    return problem(
                        "duration_ms",
                        "a timeline that loops must be at least 100 ms long",
                    );
                }
                Some(self.duration_ms)
            }
            TimelineLoop::Range { from_ms, to_ms } => {
                if !(from_ms < to_ms && to_ms <= self.duration_ms) {
                    return problem("loop", "a range needs from_ms < to_ms <= duration_ms");
                }
                if to_ms - from_ms < MIN_LOOP_MS {
                    return problem("loop", "a range must be at least 100 ms long");
                }
                Some(to_ms)
            }
        };
        if self.cues.len() > MAX_CUES {
            return problem("cues", "a timeline holds at most 256 cues");
        }
        for (at, cue) in self.cues.iter().enumerate() {
            if at > 0 && cue.at_ms < self.cues[at - 1].at_ms {
                return problem(
                    &format!("cues.{at}.at_ms"),
                    "cues must be in time order: earlier than the cue before it",
                );
            }
            let reachable = match end {
                None => cue.at_ms <= self.duration_ms,
                Some(end) => cue.at_ms < end,
            };
            if !reachable {
                return problem(
                    &format!("cues.{at}.at_ms"),
                    match self.looping {
                        TimelineLoop::Once => "is past the timeline's duration_ms",
                        TimelineLoop::Loop => {
                            "would never fire: a loop jumps back at duration_ms, so put it at 0"
                        }
                        TimelineLoop::Range { .. } => {
                            "would never fire: the loop jumps back at to_ms"
                        }
                    },
                );
            }
            let Some(action) = cue.action() else {
                return problem(
                    &format!("cues.{at}.action"),
                    "is not an action this version knows",
                );
            };
            if let Some((field, reason)) = action.cue_problem() {
                return Some((format!("cues.{at}.action.{field}"), reason.to_owned()));
            }
        }
        if self.lanes.len() > MAX_LANES {
            return problem("lanes", "a timeline holds at most 32 lanes");
        }
        for (n, lane) in self.lanes.iter().enumerate() {
            if let Err(reason) = check_channel_id(&lane.target.channel) {
                return problem(&format!("lanes.{n}.target.channel"), reason);
            }
            if lane.points.len() > MAX_LANE_POINTS {
                return problem(
                    &format!("lanes.{n}.points"),
                    "a lane holds at most 256 points",
                );
            }
            for (at, point) in lane.points.iter().enumerate() {
                let field = |name: &str| format!("lanes.{n}.points.{at}.{name}");
                if at > 0 && point.at_ms < lane.points[at - 1].at_ms {
                    return problem(
                        &field("at_ms"),
                        "points must be in time order: earlier than the point before it",
                    );
                }
                if point.at_ms > self.duration_ms {
                    return problem(&field("at_ms"), "is past the timeline's duration_ms");
                }
                if !(0.0..=1.0).contains(&point.value.get()) {
                    return problem(&field("value"), "must be 0 to 1");
                }
                if point.curve == LaneCurve::Unknown {
                    return problem(&field("curve"), "is not a curve this version knows");
                }
            }
        }
        for (list, triggers) in [("starts", &self.starts), ("stops", &self.stops)] {
            if triggers.len() > MAX_TRIGGERS {
                return problem(list, "at most 16 triggers");
            }
            for (n, trigger) in triggers.iter().enumerate() {
                if let Some((field, reason)) = trigger.problem() {
                    return problem(&format!("{list}.{n}.{field}"), reason);
                }
                if let TimelineTrigger::Timeline { timeline_id, .. } = trigger
                    && *timeline_id == self.timeline_id
                {
                    return problem(
                        &format!("{list}.{n}.timeline_id"),
                        "a timeline can't start or stop itself: use its loop",
                    );
                }
                if let TimelineTrigger::Channel { channel_id, .. } = trigger
                    && list == "starts"
                    && self
                        .lanes
                        .iter()
                        .any(|lane| lane.target.channel == *channel_id)
                {
                    return problem(
                        &format!("{list}.{n}.channel_id"),
                        "a timeline can't be started by a channel one of its own lanes drives",
                    );
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ItemId;

    fn show(at_ms: u32) -> TimelineCue {
        TimelineCue::new(
            at_ms,
            &Action::ItemShow {
                item_id: ItemId::new("i"),
                scene_id: None,
            },
        )
    }

    fn timeline(duration_ms: u32, looping: TimelineLoop, at: &[u32]) -> TimelineInfo {
        TimelineInfo {
            lanes: Vec::new(),
            starts: Vec::new(),
            stops: Vec::new(),
            labels: Vec::new(),
            timeline_id: TimelineId::new("t"),
            name: String::new(),
            duration_ms,
            looping,
            cues: at.iter().copied().map(show).collect(),
        }
    }

    fn field(t: &TimelineInfo) -> Option<String> {
        t.problem().map(|(field, _)| field)
    }

    #[test]
    fn a_sequence_in_time_order_is_kept() {
        let t = timeline(4000, TimelineLoop::Once, &[0, 1500, 1500, 4000]);
        assert_eq!(t.problem(), None);
    }

    #[test]
    fn cues_out_of_order_or_past_the_end_are_refused() {
        let t = timeline(4000, TimelineLoop::Once, &[1500, 0]);
        assert_eq!(field(&t).as_deref(), Some("cues.1.at_ms"));
        let t = timeline(4000, TimelineLoop::Once, &[4001]);
        assert_eq!(field(&t).as_deref(), Some("cues.0.at_ms"));
        let t = timeline(1000, TimelineLoop::Loop, &[0, 1000]);
        assert_eq!(field(&t).as_deref(), Some("cues.1.at_ms"));
        let range = TimelineLoop::Range {
            from_ms: 200,
            to_ms: 800,
        };
        let t = timeline(1000, range, &[0, 799, 800]);
        assert_eq!(field(&t).as_deref(), Some("cues.2.at_ms"));
    }

    #[test]
    fn lengths_ids_and_ranges_are_checked() {
        assert_eq!(
            field(&timeline(0, TimelineLoop::Loop, &[])).as_deref(),
            Some("duration_ms")
        );
        assert_eq!(
            field(&timeline(MIN_LOOP_MS - 1, TimelineLoop::Loop, &[])).as_deref(),
            Some("duration_ms")
        );
        assert_eq!(
            field(&timeline(MIN_LOOP_MS, TimelineLoop::Loop, &[])).as_deref(),
            None
        );
        let short = TimelineLoop::Range {
            from_ms: 800,
            to_ms: 800 + MIN_LOOP_MS - 1,
        };
        assert_eq!(field(&timeline(1000, short, &[])).as_deref(), Some("loop"));
        assert_eq!(
            field(&timeline(MAX_TIMELINE_MS + 1, TimelineLoop::Once, &[])).as_deref(),
            Some("duration_ms")
        );
        let range = TimelineLoop::Range {
            from_ms: 800,
            to_ms: 800,
        };
        assert_eq!(field(&timeline(1000, range, &[])).as_deref(), Some("loop"));
        let mut t = timeline(1000, TimelineLoop::Once, &[]);
        t.timeline_id = TimelineId::new("has space");
        assert_eq!(field(&t).as_deref(), Some("timeline_id"));
        let mut t = timeline(1000, TimelineLoop::Once, &[]);
        t.cues = (0..=MAX_CUES as u32).map(show).collect();
        assert_eq!(field(&t).as_deref(), Some("cues"));
    }

    #[test]
    fn a_cue_runs_only_an_allowed_action() {
        let mut t = timeline(1000, TimelineLoop::Once, &[]);
        t.cues.push(TimelineCue::new(
            0,
            &Action::OutputStart {
                output_id: crate::id::OutputId::new("o"),
            },
        ));
        assert_eq!(field(&t).as_deref(), Some("cues.0.action.type"));
        t.cues[0].action = serde_json::json!({"type": "a.later_action"});
        assert_eq!(field(&t).as_deref(), Some("cues.0.action"));
    }

    #[test]
    fn the_wire_form_names_the_loop() {
        let t = timeline(
            1000,
            TimelineLoop::Range {
                from_ms: 200,
                to_ms: 800,
            },
            &[0],
        );
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(
            json["loop"],
            serde_json::json!({"range": {"from_ms": 200, "to_ms": 800}})
        );
        assert_eq!(json["cues"][0]["action"]["type"], "item.show");
        let once: TimelineInfo =
            serde_json::from_value(serde_json::json!({"timeline_id": "t", "duration_ms": 5}))
                .unwrap();
        assert_eq!(once.looping, TimelineLoop::Once);
        assert!(once.cues.is_empty());
    }

    fn point(at_ms: u32, value: f32, curve: LaneCurve) -> LanePoint {
        LanePoint {
            at_ms,
            value: Finite32::new(value).expect("finite"),
            curve,
        }
    }

    fn lane(points: Vec<LanePoint>) -> TimelineLane {
        TimelineLane {
            target: LaneTarget {
                channel: "lights".into(),
            },
            points,
            on: true,
        }
    }

    /// ADR-0184 rule 2: a lane with no point at 0 starts from where the channel was.
    #[test]
    fn a_lane_starts_from_the_value_given_and_holds_its_last_point() {
        let down = lane(vec![point(3000, 0.0, LaneCurve::Linear)]);
        assert_eq!(down.value_at(0.0, 0.6), 0.6);
        assert!((down.value_at(1500.0, 0.6) - 0.3).abs() < 1e-6);
        assert_eq!(down.value_at(5000.0, 0.6), 0.0);
        let switch = lane(vec![
            point(0, 0.0, LaneCurve::Linear),
            point(1000, 1.0, LaneCurve::Hold),
        ]);
        assert_eq!(
            switch.value_at(0.0, 0.9),
            0.0,
            "a point at 0 sets where it starts"
        );
        assert_eq!(switch.value_at(999.0, 0.9), 0.0, "hold waits");
        assert_eq!(switch.value_at(1000.0, 0.9), 1.0, "then jumps");
        let eased = lane(vec![point(1000, 1.0, LaneCurve::EaseIn)]);
        assert!((eased.value_at(500.0, 0.0) - 0.25).abs() < 1e-6);
        assert_eq!(lane(Vec::new()).value_at(10.0, 0.4), 0.4);
    }

    #[test]
    fn lanes_and_triggers_are_checked() {
        let mut t = timeline(2000, TimelineLoop::Once, &[]);
        t.lanes = vec![lane(vec![point(500, 0.5, LaneCurve::Linear)])];
        assert_eq!(field(&t), None);
        t.lanes[0].points.push(point(400, 0.5, LaneCurve::Linear));
        assert_eq!(field(&t).as_deref(), Some("lanes.0.points.1.at_ms"));
        t.lanes[0].points = vec![point(2500, 0.5, LaneCurve::Linear)];
        assert_eq!(field(&t).as_deref(), Some("lanes.0.points.0.at_ms"));
        t.lanes[0].points = vec![point(500, 1.5, LaneCurve::Linear)];
        assert_eq!(field(&t).as_deref(), Some("lanes.0.points.0.value"));
        t.lanes[0].points = Vec::new();
        t.starts = vec![TimelineTrigger::Channel {
            channel_id: "lights".into(),
            edge: ChannelEdge::Rises,
            level: None,
            delay_ms: 0,
        }];
        assert_eq!(
            field(&t).as_deref(),
            Some("starts.0.channel_id"),
            "its own lane"
        );
        t.starts = vec![TimelineTrigger::Timeline {
            timeline_id: TimelineId::new("t"),
            edge: TimelineEdge::Ends,
            delay_ms: 0,
        }];
        assert_eq!(field(&t).as_deref(), Some("starts.0.timeline_id"), "itself");
        t.starts = vec![TimelineTrigger::Scene {
            scene_id: SceneId::new("s"),
            edge: SceneEdge::Shown,
            delay_ms: 60_001,
        }];
        assert_eq!(field(&t).as_deref(), Some("starts.0.delay_ms"));
        t.starts.clear();
        t.stops = vec![TimelineTrigger::Channel {
            channel_id: "lights".into(),
            edge: ChannelEdge::Falls,
            level: Some(Finite32::new(2.0).expect("finite")),
            delay_ms: 0,
        }];
        assert_eq!(field(&t).as_deref(), Some("stops.0.level"));
    }

    #[test]
    fn a_play_waits_at_most_a_minute() {
        let play = |delay_ms| Action::TimelinePlay {
            timeline_id: TimelineId::new("t"),
            delay_ms,
        };
        assert_eq!(play(Some(60_000)).play_problem(), None);
        assert_eq!(
            play(Some(60_001)).play_problem().map(|p| p.0),
            Some("delay_ms")
        );
        assert_eq!(
            play(Some(60_001)).cue_problem().map(|p| p.0),
            Some("delay_ms")
        );
    }
}
