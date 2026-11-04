//! Timelines on the wire ([ADR-0099](../../../docs/adr/0099-native-motion-modifiers-and-timelines.md)
//! decision 4, [ADR-0114](../../../docs/adr/0114-timelines-on-the-wire.md)): timed cues a
//! scene holds, each running a protocol action at a time from the timeline's start.
//!
//! A scene holds its timelines in `SceneInfo.timelines`, which `timeline.set` and
//! `timeline.delete` edit. What is playing is studio state, like the program scene:
//! `timeline.play`, `timeline.stop` and `timeline.seek` are actions, and `studio.get`
//! reports the timelines playing. Each cue's action is one of a closed set
//! ([`Action::cue_problem`]), so playing a timeline never makes an edit.

use crate::command::Action;
use crate::id::{SceneId, TimelineId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most timelines one scene holds (ADR-0099 decision 5).
pub const MAX_TIMELINES: usize = 32;
/// The most cues one timeline holds (ADR-0099 decision 5).
pub const MAX_CUES: usize = 256;
/// The longest a timeline runs, in milliseconds: one hour (ADR-0099 decision 5).
pub const MAX_TIMELINE_MS: u32 = 3_600_000;
/// The longest timeline ID, in bytes.
pub const MAX_TIMELINE_ID_LEN: usize = 64;
/// The longest timeline name, in bytes.
pub const MAX_TIMELINE_NAME_LEN: usize = 256;

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
}

/// How a timeline repeats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimelineLoop {
    /// It plays once, and ends at its `duration_ms`. A cue at `duration_ms` fires.
    #[default]
    Once,
    /// It plays from 0 to `duration_ms` again and again, until stopped. A cue at
    /// `duration_ms` would never fire, so it is refused: put it at 0.
    Loop,
    /// It plays from 0, and each time it reaches `to_ms` jumps back to `from_ms`: an
    /// intro, then a loop. `from_ms < to_ms <= duration_ms`, and a cue at or after
    /// `to_ms` is refused.
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
    /// What it does: one of the actions [`Action::cue_problem`] allows.
    pub action: Action,
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
                if self.duration_ms == 0 {
                    return problem("duration_ms", "a timeline that loops must be longer than 0");
                }
                Some(self.duration_ms)
            }
            TimelineLoop::Range { from_ms, to_ms } => {
                if !(from_ms < to_ms && to_ms <= self.duration_ms) {
                    return problem("loop", "a range needs from_ms < to_ms <= duration_ms");
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
            if let Some((field, reason)) = cue.action.cue_problem() {
                return Some((format!("cues.{at}.action.{field}"), reason.to_owned()));
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
        TimelineCue {
            at_ms,
            action: Action::ItemShow {
                item_id: ItemId::new("i"),
                scene_id: None,
            },
        }
    }

    fn timeline(duration_ms: u32, looping: TimelineLoop, at: &[u32]) -> TimelineInfo {
        TimelineInfo {
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
        t.cues.push(TimelineCue {
            at_ms: 0,
            action: Action::OutputStart {
                output_id: crate::id::OutputId::new("o"),
            },
        });
        assert_eq!(field(&t).as_deref(), Some("cues.0.action.type"));
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
}
