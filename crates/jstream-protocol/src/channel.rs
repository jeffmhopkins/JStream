//! Channels ([ADR-0183](../../../docs/adr/0183-virtual-control-channels.md), since 0.169):
//! named live values, 0 to 1, that the collection defines, controls and actions set, and
//! anything a modifier or value binding drives can follow.
//!
//! A channel is a collection object (`channel.set`, `channel.delete`, listed in
//! `state.get`'s `channels` and saved in the file). Its value is runtime state, like the
//! program scene: `channel.set_value`, `channel.switch` and `channel.nudge` set it, and
//! `channel.values` reads it; none of them is an edit, so moving a channel never fills an
//! undo history.

use crate::finite::Finite32;
use crate::id::SceneId;
use crate::motion::{MotionCurve, MotionKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most channels one collection holds.
pub const MAX_CHANNELS: usize = 256;
/// The most inputs one channel reads.
pub const MAX_CHANNEL_INPUTS: usize = 8;
/// The longest channel ID, in bytes.
pub const MAX_CHANNEL_ID_LEN: usize = 64;
/// The longest channel name, in bytes.
pub const MAX_CHANNEL_NAME_LEN: usize = 256;
/// The longest a channel's rise or fall, or an action's fade, takes, in milliseconds.
pub const MAX_CHANNEL_FADE_MS: u32 = 60_000;
/// The longest window a channel averages over, in milliseconds.
pub const MAX_CHANNEL_AVERAGE_MS: u32 = 10_000;

/// Checks a channel ID: 1 to [`MAX_CHANNEL_ID_LEN`] bytes of ASCII letters, digits, `_`
/// and `-`. Returns what it must be if it isn't.
pub fn check_channel_id(id: &str) -> Result<(), &'static str> {
    let well_formed = !id.is_empty()
        && id.len() <= MAX_CHANNEL_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if well_formed {
        Ok(())
    } else {
        Err("must be 1 to 64 bytes of ASCII letters, digits, `_` and `-`")
    }
}

/// How a channel makes one value of its inputs and the value actions set (ADR-0183
/// decision 4). Only inputs that have moved since the engine started following them take
/// part, and the actions' value once an action has set it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChannelCombine {
    /// Whichever changed last, as a DAW's pickup takes a moved control.
    #[default]
    Latest,
    /// The largest.
    Max,
    /// The least.
    Min,
    /// The sum, kept within 0 to 1.
    Sum,
    /// The product, so one input gates another.
    Product,
    /// The mean.
    Average,
    /// A value a newer server sends that this version doesn't know. Never sent; the
    /// engine refuses it in an edit with `invalid_argument`.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// A channel: a named value, 0 to 1, with the inputs that set it and how it processes
/// them (ADR-0183 decision 1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelInfo {
    /// Its ID, unique among the collection's channels: 1 to 64 bytes of ASCII letters,
    /// digits, `_` and `-`.
    pub channel_id: String,
    /// Its display name, at most 256 bytes. Omitted means empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The scene it belongs to, or omitted for the whole collection. It returns to its
    /// `default` and `on` each time that scene goes on program. The scene need not exist:
    /// deleting it leaves the channel in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<SceneId>,
    /// Its value when the engine starts following it, before any input moves or action
    /// sets it, 0 to 1. Default 0.
    #[serde(default)]
    pub default: Finite32,
    /// What sets it: at most 8 readers, each a `midi`, `data`, `channel` or (since 0.177,
    /// ADR-0206) `audio` modifier's kind and parameters, whose `from` and `to` map the
    /// reading onto the channel's 0 to 1. An `audio` reader's attack and release follow
    /// its level, as on an item.
    /// A `channel` input can't lead back to this channel. Default none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<MotionKind>,
    /// How its inputs and actions make one value. Default `latest`.
    #[serde(default)]
    pub combine: ChannelCombine,
    /// A moving average over this many milliseconds, `0..=10000`; 0 averages nothing.
    /// Default 0.
    #[serde(default)]
    pub average_ms: u32,
    /// How long a full rise from 0 to 1 takes, in milliseconds, `0..=60000`, as a
    /// straight ramp; 0 jumps. Default 0.
    #[serde(default)]
    pub rise_ms: u32,
    /// How long a full fall from 1 to 0 takes, as `rise_ms`. Default 0.
    #[serde(default)]
    pub fall_ms: u32,
    /// How its value is bent, last. Default `linear`.
    #[serde(default)]
    pub curve: MotionCurve,
    /// Whether it starts switched on. Default `true`; omitted when `true`.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub on: bool,
    /// What it reads while switched off, 0 to 1, through its fade. Default 0.
    #[serde(default)]
    pub off_value: Finite32,
}

pub(crate) fn yes() -> bool {
    true
}

pub(crate) fn is_true(value: &bool) -> bool {
    *value
}

impl ChannelInfo {
    /// A channel with `channel_id` and every other field at its default.
    pub fn new(channel_id: impl Into<String>) -> Self {
        ChannelInfo {
            channel_id: channel_id.into(),
            name: String::new(),
            scene_id: None,
            default: Finite32::ZERO,
            inputs: Vec::new(),
            combine: ChannelCombine::Latest,
            average_ms: 0,
            rise_ms: 0,
            fall_ms: 0,
            curve: MotionCurve::Linear,
            on: true,
            off_value: Finite32::ZERO,
        }
    }

    /// The channels its `channel` inputs read, in input order.
    pub fn reads(&self) -> impl Iterator<Item = &str> {
        self.inputs.iter().filter_map(|input| match input {
            MotionKind::Channel { channel_id, .. } => Some(channel_id.as_str()),
            _ => None,
        })
    }

    /// Why this is not a valid channel, as the field and the reason, or `None` if it is.
    /// Whether a `channel` input leads back to it is the collection's to check.
    pub fn problem(&self) -> Option<(String, String)> {
        let problem = |field: &str, reason: &str| Some((field.to_owned(), reason.to_owned()));
        let unit = |v: Finite32| (0.0..=1.0).contains(&v.get());
        if let Err(reason) = check_channel_id(&self.channel_id) {
            return problem("channel_id", reason);
        }
        if self.name.len() > MAX_CHANNEL_NAME_LEN {
            return problem("name", "must be at most 256 bytes");
        }
        if !unit(self.default) {
            return problem("default", "must be within 0..=1");
        }
        if !unit(self.off_value) {
            return problem("off_value", "must be within 0..=1");
        }
        if self.inputs.len() > MAX_CHANNEL_INPUTS {
            return problem("inputs", "a channel reads at most 8 inputs");
        }
        for (n, input) in self.inputs.iter().enumerate() {
            if !matches!(
                input,
                MotionKind::Midi { .. }
                    | MotionKind::Data { .. }
                    | MotionKind::Channel { .. }
                    | MotionKind::Audio { .. }
            ) {
                return Some((
                    format!("inputs.{n}.kind"),
                    "a channel reads a `midi` control, a `data` field, another `channel` or an `audio` level".into(),
                ));
            }
            if matches!(
                input,
                MotionKind::Data {
                    color_from: Some(_),
                    ..
                }
            ) {
                return Some((
                    format!("inputs.{n}.color_from"),
                    "a channel's value is a number: leave `color_from` out".into(),
                ));
            }
            if let MotionKind::Channel { channel_id, .. } = input
                && *channel_id == self.channel_id
            {
                return Some((
                    format!("inputs.{n}.channel_id"),
                    "a channel can't read itself".into(),
                ));
            }
            if let Some((param, reason)) = input.problem() {
                return Some((format!("inputs.{n}.{param}"), reason.into()));
            }
        }
        if self.combine == ChannelCombine::Unknown {
            return problem("combine", "is not one this version of the protocol knows");
        }
        if self.average_ms > MAX_CHANNEL_AVERAGE_MS {
            return problem("average_ms", "must be within 0..=10000");
        }
        if self.rise_ms > MAX_CHANNEL_FADE_MS {
            return problem("rise_ms", "must be within 0..=60000");
        }
        if self.fall_ms > MAX_CHANNEL_FADE_MS {
            return problem("fall_ms", "must be within 0..=60000");
        }
        if self.curve == MotionCurve::Unknown {
            return problem("curve", "is not one this version of the protocol knows");
        }
        None
    }
}

/// What `channel.switch` does to a channel's switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelSwitch {
    /// Switches it on.
    On,
    /// Switches it off: it reads its `off_value`, through its fade.
    Off,
    /// Switches it on if it is off, and off if it is on.
    Toggle,
}

/// One channel's value now, in `channel.values`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelValueInfo {
    /// The channel.
    pub channel_id: String,
    /// Its value now, 0 to 1, after its processing.
    pub value: Finite32,
    /// Whether it is switched on.
    pub on: bool,
    /// What sets it now, under its `combine`: the input or action that changed last for
    /// `latest`, and for the others the one whose value it takes (`max`, `min`) or the
    /// last to change. Omitted while nothing has moved it and it reads its `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<ChannelDriver>,
}

/// What sets a channel's value now, in `channel.values`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "by", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChannelDriver {
    /// One of its inputs, by its index in `inputs`.
    Input {
        /// The input's index.
        index: u8,
    },
    /// `channel.set_value` or `channel.nudge`, from a client, a hotkey, a binding or a
    /// cue.
    Action,
    /// A lane of a playing (or played) sequence: a timeline's lane on this channel
    /// (ADR-0184, since 0.170).
    Sequence {
        /// The timeline whose lane drives it.
        timeline_id: crate::TimelineId,
    },
    /// A value a newer server sends that this version doesn't know.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn midi(controller: u8) -> MotionKind {
        MotionKind::Midi {
            control: crate::MotionMidiControl::Cc,
            controller: Some(controller),
            channel: None,
            input: None,
            smoothing_ms: 0,
            curve: MotionCurve::Linear,
            from: Finite32::ZERO,
            to: Finite32::ONE,
        }
    }

    fn reads(channel_id: &str) -> MotionKind {
        MotionKind::Channel {
            channel_id: channel_id.into(),
            smoothing_ms: 0,
            curve: MotionCurve::Linear,
            from: Finite32::ZERO,
            to: Finite32::ONE,
        }
    }

    #[test]
    fn a_channel_round_trips_with_its_defaults_left_out() {
        let channel = ChannelInfo::new("power");
        let json = serde_json::to_value(&channel).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!({"channel_id": "power", "default": 0.0,
            "combine": "latest", "average_ms": 0, "rise_ms": 0, "fall_ms": 0,
            "curve": "linear", "off_value": 0.0})
        );
        let back: ChannelInfo =
            serde_json::from_value(serde_json::json!({"channel_id": "power"})).expect("parses");
        assert_eq!(back, channel);
        let mut fed = channel.clone();
        fed.inputs = vec![midi(21), reads("master")];
        let json = serde_json::to_value(&fed).expect("serializes");
        assert_eq!(json["inputs"][0]["kind"], "midi");
        assert_eq!(json["inputs"][1]["kind"], "channel");
        let back: ChannelInfo = serde_json::from_value(json).expect("parses");
        assert_eq!(back, fed);
        assert_eq!(back.reads().collect::<Vec<_>>(), ["master"]);
    }

    #[test]
    fn a_channel_refuses_what_it_cant_hold() {
        let ok = ChannelInfo::new("lights");
        assert_eq!(ok.problem(), None);
        let field = |c: ChannelInfo| c.problem().map(|(f, _)| f);
        assert_eq!(field(ChannelInfo::new("")), Some("channel_id".into()));
        assert_eq!(
            field(ChannelInfo {
                default: Finite32::new(1.5).expect("finite"),
                ..ok.clone()
            }),
            Some("default".into())
        );
        assert_eq!(
            field(ChannelInfo {
                inputs: vec![midi(1); 9],
                ..ok.clone()
            }),
            Some("inputs".into())
        );
        assert_eq!(
            field(ChannelInfo {
                inputs: vec![reads("lights")],
                ..ok.clone()
            }),
            Some("inputs.0.channel_id".into())
        );
        assert_eq!(
            field(ChannelInfo {
                inputs: vec![MotionKind::Drift {
                    rate_per_s: Finite32::ONE,
                    wrap: None
                }],
                ..ok.clone()
            }),
            Some("inputs.0.kind".into())
        );
        assert_eq!(
            field(ChannelInfo {
                rise_ms: 60_001,
                ..ok.clone()
            }),
            Some("rise_ms".into())
        );
        assert_eq!(
            field(ChannelInfo {
                average_ms: 10_001,
                ..ok
            }),
            Some("average_ms".into())
        );
    }
}
