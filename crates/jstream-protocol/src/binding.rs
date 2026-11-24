//! Bindings of controller triggers (since 0.96): a MIDI note, control change or program
//! change bound to a protocol action, kept in the collection beside the hotkeys
//! (docs/adr/0122-controller-input-midi-and-one-binding-system.md decision 1).
//!
//! A key chord stays a hotkey (`hotkey.set`, [`HotkeyBinding`](crate::HotkeyBinding)),
//! so a client that registers chords with the OS reads the same `hotkeys` list it always
//! did. Every other trigger is a binding, set with `binding.set`. Unlike a chord, which a
//! client listens for and answers with an `action` call, a MIDI trigger comes from an
//! input the engine reads itself, so the engine runs the action. Every binding runs a
//! protocol action, so a controller can do nothing an agent can't (R-ARCH-001).

use crate::command::Action;
use crate::filter::FilterOwner;
use crate::id::{BusId, FilterId, SourceId};
use crate::motion::{MAX_MOTION_PARAM_LEN, MotionKind, check_motion_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The longest input name a trigger may name, in bytes.
pub const MAX_INPUT_NAME_LEN: usize = 64;

/// What fires a binding: one MIDI message, from one input or any, on one channel or any.
///
/// - A note fires on note-on (a note-on with velocity 0 is a note-off, and never fires).
/// - A control change fires when its value crosses the middle upward, from 63 or below
///   to 64 or above, so a button that sends 127 then 0 fires once per press.
/// - A program change fires each time it arrives.
///
/// `channel` is 1 to 16, as a MIDI device's panel shows it, or missing for any channel.
/// `input` is the name of the input (a MIDI port, or a JTunnel sender), or missing for
/// any. Two triggers are the same exactly when they are equal, so `channel: 1` and any
/// channel are two triggers: a message that matches both fires both.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    /// A note-on.
    MidiNote {
        /// The note number, 0 to 127 (60 is middle C).
        note: u8,
        /// The channel, 1 to 16, or missing for any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// The input, or missing for any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
    },
    /// A control change crossing the middle upward.
    MidiCc {
        /// The controller number, 0 to 127.
        controller: u8,
        /// The channel, 1 to 16, or missing for any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// The input, or missing for any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
    },
    /// A program change.
    MidiProgram {
        /// The program number, 0 to 127, as sent (a device that shows programs from 1
        /// sends its program 1 as 0).
        program: u8,
        /// The channel, 1 to 16, or missing for any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
        /// The input, or missing for any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
    },
}

impl Trigger {
    /// Why this is not a valid trigger, as the field and the reason, or `None` if it is.
    /// The engine rejects a trigger with this reason, so every implementation gives the
    /// same one.
    pub fn problem(&self) -> Option<(&'static str, String)> {
        let (field, number, channel, input) = match self {
            Trigger::MidiNote {
                note,
                channel,
                input,
            } => ("note", *note, *channel, input),
            Trigger::MidiCc {
                controller,
                channel,
                input,
            } => ("controller", *controller, *channel, input),
            Trigger::MidiProgram {
                program,
                channel,
                input,
            } => ("program", *program, *channel, input),
        };
        if number > 127 {
            return Some((field, "must be 0 to 127".into()));
        }
        if let Some(c) = channel
            && !(1..=16).contains(&c)
        {
            return Some((
                "channel",
                "must be 1 to 16, or missing for any channel".into(),
            ));
        }
        if let Some(name) = input {
            if name.is_empty() {
                return Some((
                    "input",
                    "must not be empty: leave it out for any input".into(),
                ));
            }
            if name.len() > MAX_INPUT_NAME_LEN {
                return Some((
                    "input",
                    format!("must be at most {MAX_INPUT_NAME_LEN} bytes"),
                ));
            }
            if name.chars().any(char::is_control) {
                return Some(("input", "must not hold control characters".into()));
            }
        }
        None
    }

    /// The input the trigger is limited to, if any.
    pub fn input(&self) -> Option<&str> {
        match self {
            Trigger::MidiNote { input, .. }
            | Trigger::MidiCc { input, .. }
            | Trigger::MidiProgram { input, .. } => input.as_deref(),
        }
    }

    /// The channel the trigger is limited to, 1 to 16, if any.
    pub fn channel(&self) -> Option<u8> {
        match self {
            Trigger::MidiNote { channel, .. }
            | Trigger::MidiCc { channel, .. }
            | Trigger::MidiProgram { channel, .. } => *channel,
        }
    }
}

impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (what, n) = match self {
            Trigger::MidiNote { note, .. } => ("note", note),
            Trigger::MidiCc { controller, .. } => ("CC", controller),
            Trigger::MidiProgram { program, .. } => ("program", program),
        };
        write!(f, "MIDI {what} {n}")?;
        match self.channel() {
            Some(c) => write!(f, " on channel {c}")?,
            None => f.write_str(" on any channel")?,
        }
        if let Some(input) = self.input() {
            write!(f, " from {input}")?;
        }
        Ok(())
    }
}

/// One binding, as `state.get`, `delete.plan` and a batch result report it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Binding {
    /// What fires it.
    pub trigger: Trigger,
    /// The action request it runs, as `binding.set` bound it. Read as plain JSON, so a
    /// client older than an action type still reads the collection: it keeps the
    /// binding and can't run it. [`Binding::action`] parses it.
    #[schemars(with = "Action")]
    pub action: serde_json::Value,
}

impl Binding {
    /// A binding of `trigger` to `action`.
    pub fn new(trigger: Trigger, action: &Action) -> Self {
        Binding {
            trigger,
            // An action is plain data: IDs, strings and a secret string, which always
            // serialize. A stream key is never bound, so none is copied here.
            action: serde_json::to_value(action).expect("invariant: an Action serializes to JSON"),
        }
    }

    /// The bound action, or `None` when it is one this version doesn't know.
    pub fn action(&self) -> Option<Action> {
        serde_json::from_value(self.action.clone()).ok()
    }
}

/// One MIDI input's latest control change, in `midi.last_controls` (since 0.168,
/// ADR-0178).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MidiControlInfo {
    /// The input, a MIDI port or a JTunnel sender by name, as a binding names it.
    pub input: String,
    /// Its channel, 1 to 16.
    pub channel: u8,
    /// Its controller number, 0 to 127.
    pub controller: u8,
    /// The controller's value now, 0 to 127.
    pub value: u8,
    /// How many control changes the input has sent since the engine started: a client
    /// learning a controller waits for it to change.
    pub count: u64,
}

/// How many value bindings a collection holds (ADR-0178). One more is
/// `invalid_argument`.
pub const MAX_VALUE_BINDINGS: usize = 256;

/// The number a value binding drives (ADR-0178, since 0.168): one setting anywhere in
/// the collection that isn't an item's or a camera's, which their own motion modifiers
/// drive. Serialized with an internal `type` tag.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ValueTarget {
    /// A `live` number or `bool` setting of a source, which every place that draws it
    /// shows.
    SourceSetting {
        /// The source.
        source_id: SourceId,
        /// The setting, by its ID in the kind's descriptor.
        param: String,
    },
    /// A `live` number or `bool` setting of a filter, on any chain: a source's, a
    /// scene's, an item's effects, a keyframe's, the master stage's or an output's, video
    /// or audio.
    FilterSetting {
        /// What owns the filter.
        owner: FilterOwner,
        /// The filter.
        filter_id: FilterId,
        /// The setting, by its ID in the kind's descriptor.
        param: String,
    },
    /// An audio source's fader, in dB.
    SourceGain {
        /// The source.
        source_id: SourceId,
    },
    /// A bus's fader, in dB, the master's included.
    BusGain {
        /// The bus.
        bus_id: BusId,
    },
    /// The gain of a source's send into a bus, in dB.
    SendGain {
        /// The source sending.
        source_id: SourceId,
        /// The bus it sends into.
        bus_id: BusId,
    },
    /// One axis of a PTZ camera's position (ADR-0209, since 0.180), in the head's own
    /// counts (ADR-0146 §5): the engine aims the head there as the value moves. The
    /// camera is named as `ptz.*` actions and shots name it, so a collection carries
    /// the mapping to a machine that lists a camera by that name; on a machine that
    /// doesn't, it drives nothing.
    PtzAxis {
        /// The camera's name on the machine's PTZ camera list.
        camera: String,
        /// Which axis.
        axis: PtzAxis,
    },
}

/// An axis of a PTZ head's position that a value binding drives (ADR-0209, since 0.180).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PtzAxis {
    /// The pan position, -524287 to 524287.
    Pan,
    /// The tilt position, -32767 to 32767.
    Tilt,
    /// The zoom position, 0 (the widest) to 65535.
    Zoom,
    /// The focus position, 0 to 65535. Driving it puts the head's focus in manual.
    Focus,
}

impl PtzAxis {
    /// Every axis, in this order.
    pub const ALL: [PtzAxis; 4] = [PtzAxis::Pan, PtzAxis::Tilt, PtzAxis::Zoom, PtzAxis::Focus];

    /// The lowest and highest counts the protocol carries for it. A head's own range is
    /// usually narrower: a mapping's `from` and `to` pick the part it uses.
    pub fn range(self) -> (i32, i32) {
        match self {
            PtzAxis::Pan => (-crate::ptz::MAX_PTZ_PAN, crate::ptz::MAX_PTZ_PAN),
            PtzAxis::Tilt => (-crate::ptz::MAX_PTZ_TILT, crate::ptz::MAX_PTZ_TILT),
            PtzAxis::Zoom | PtzAxis::Focus => (0, i32::from(u16::MAX)),
        }
    }

    /// Its name on the wire.
    pub fn name(self) -> &'static str {
        match self {
            PtzAxis::Pan => "pan",
            PtzAxis::Tilt => "tilt",
            PtzAxis::Zoom => "zoom",
            PtzAxis::Focus => "focus",
        }
    }
}

impl ValueTarget {
    /// The parameter it names, for a setting.
    pub fn param(&self) -> Option<&str> {
        match self {
            ValueTarget::SourceSetting { param, .. } | ValueTarget::FilterSetting { param, .. } => {
                Some(param)
            }
            ValueTarget::SourceGain { .. }
            | ValueTarget::BusGain { .. }
            | ValueTarget::SendGain { .. }
            | ValueTarget::PtzAxis { .. } => None,
        }
    }
}

impl fmt::Display for ValueTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueTarget::SourceSetting { source_id, param } => {
                write!(f, "setting {param} of source {source_id}")
            }
            ValueTarget::FilterSetting {
                filter_id, param, ..
            } => write!(f, "setting {param} of filter {filter_id}"),
            ValueTarget::SourceGain { source_id } => write!(f, "the fader of source {source_id}"),
            ValueTarget::BusGain { bus_id } => write!(f, "the fader of bus {bus_id}"),
            ValueTarget::SendGain { source_id, bus_id } => {
                write!(f, "the send of source {source_id} into bus {bus_id}")
            }
            ValueTarget::PtzAxis { camera, axis } => {
                write!(f, "the {} of PTZ camera {camera}", axis.name())
            }
        }
    }
}

/// A value binding (ADR-0122 decision 1, ADR-0178, since 0.168): a MIDI controller or a
/// field of a sender's live data, read as a `midi` or `data` motion modifier reads it
/// (smoothed, bent and mapped onto `from..to`), driving one number anywhere in the
/// collection. Its value replaces the stored one as it is drawn and heard, kept within
/// the setting's range (rounded for a whole number, on at one half or more for a
/// `bool`), from the moment the control first moves after the engine starts or the
/// binding is set, so a collection opens as it was saved. Nothing is stored and no
/// undo history fills.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ValueBinding {
    /// Its ID, unique among the collection's value bindings: 1 to 64 bytes of ASCII
    /// letters, digits, `_` and `-`.
    pub binding_id: String,
    /// What it reads: a `midi`, `data` or (since 0.169) `channel` modifier's kind and
    /// parameters.
    #[serde(flatten)]
    pub input: MotionKind,
    /// What it drives.
    pub target: ValueTarget,
    /// A channel whose value, 0 to 1, sets how much it drives (ADR-0183 decision 7,
    /// since 0.169): its mapped value is pulled toward `from` as
    /// `from + (value - from) * amount`. A channel the collection doesn't hold drives it
    /// fully. Omitted for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
}

impl ValueBinding {
    /// Why this is not a valid value binding, as the field and the reason, or `None` if
    /// it is. Whether its target exists and takes a number is the collection's to check.
    pub fn problem(&self) -> Option<(String, String)> {
        if let Err(reason) = check_motion_id(&self.binding_id) {
            return Some(("binding_id".into(), reason.into()));
        }
        if !matches!(
            self.input,
            MotionKind::Midi { .. } | MotionKind::Data { .. } | MotionKind::Channel { .. }
        ) {
            return Some((
                "kind".into(),
                "a value binding reads a `midi` control, a `data` field or a `channel`".into(),
            ));
        }
        if let Some(reason) = crate::motion::amount_problem(self.amount.as_deref()) {
            return Some(("amount".into(), reason.into()));
        }
        if let Some((field, reason)) = self.input.problem() {
            return Some((field.into(), reason.into()));
        }
        if let ValueTarget::PtzAxis { camera, .. } = &self.target
            && let Some(reason) = crate::ptz::camera_name_problem(camera)
        {
            return Some(("target.camera".into(), reason));
        }
        if let Some(param) = self.target.param()
            && (param.is_empty()
                || param.len() > MAX_MOTION_PARAM_LEN
                || !param
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        {
            return Some((
                "target.param".into(),
                "must be 1 to 64 bytes of ASCII letters, digits and `_`".into(),
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{Binding, MAX_INPUT_NAME_LEN, Trigger};
    use crate::command::Action;
    use crate::id::SceneId;
    use serde_json::json;

    fn note(note: u8, channel: Option<u8>, input: Option<&str>) -> Trigger {
        Trigger::MidiNote {
            note,
            channel,
            input: input.map(str::to_owned),
        }
    }

    #[test]
    fn triggers_have_one_spelling_on_the_wire() {
        assert_eq!(
            serde_json::to_value(note(60, None, None)).expect("serializes"),
            json!({"kind": "midi_note", "note": 60})
        );
        let cc = Trigger::MidiCc {
            controller: 64,
            channel: Some(10),
            input: Some("Launch Control".into()),
        };
        let wire = json!({"kind": "midi_cc", "controller": 64, "channel": 10,
                          "input": "Launch Control"});
        assert_eq!(serde_json::to_value(&cc).expect("serializes"), wire);
        assert_eq!(serde_json::from_value::<Trigger>(wire).ok(), Some(cc));
        assert_eq!(
            note(36, Some(10), Some("Pads")).to_string(),
            "MIDI note 36 on channel 10 from Pads"
        );
    }

    #[test]
    fn invalid_triggers_say_why() {
        assert_eq!(note(127, Some(16), Some("x")).problem(), None);
        let long = "x".repeat(MAX_INPUT_NAME_LEN + 1);
        let cases = [
            (note(128, None, None), "note", "must be 0 to 127"),
            (
                Trigger::MidiProgram {
                    program: 200,
                    channel: None,
                    input: None,
                },
                "program",
                "must be 0 to 127",
            ),
            (
                note(1, Some(0), None),
                "channel",
                "must be 1 to 16, or missing for any channel",
            ),
            (
                note(1, Some(17), None),
                "channel",
                "must be 1 to 16, or missing for any channel",
            ),
            (
                note(1, None, Some("")),
                "input",
                "must not be empty: leave it out for any input",
            ),
            (
                note(1, None, Some(&long)),
                "input",
                "must be at most 64 bytes",
            ),
            (
                note(1, None, Some("Pads\n")),
                "input",
                "must not hold control characters",
            ),
        ];
        for (trigger, field, reason) in cases {
            assert_eq!(
                trigger.problem(),
                Some((field, reason.to_owned())),
                "{trigger:?}"
            );
        }
    }

    #[test]
    fn a_binding_reads_back_its_action_and_tolerates_unknown_ones() {
        let switch = Action::SceneSwitch {
            scene_id: SceneId::new("intro"),
            preset_id: None,
            via: Vec::new(),
        };
        let binding = Binding::new(note(60, None, None), &switch);
        assert_eq!(
            serde_json::to_value(&binding).expect("serializes"),
            json!({"trigger": {"kind": "midi_note", "note": 60},
                   "action": {"type": "scene.switch", "scene_id": "intro"}})
        );
        assert_eq!(binding.action(), Some(switch));
        let later: Binding = serde_json::from_value(json!({
            "trigger": {"kind": "midi_note", "note": 1},
            "action": {"type": "studio.later"}
        }))
        .expect("an unknown action still parses");
        assert_eq!(later.action(), None);
    }

    /// ADR-0209: a PTZ axis target names a camera and an axis, on the wire as
    /// `ptz_axis`, and its camera name is checked as `ptz.*` checks one.
    #[test]
    fn a_value_binding_can_aim_a_ptz_axis() {
        use super::{PtzAxis, ValueBinding, ValueTarget};
        let target = ValueTarget::PtzAxis {
            camera: "wide".into(),
            axis: PtzAxis::Zoom,
        };
        assert_eq!(
            serde_json::to_value(&target).expect("serializes"),
            json!({"type": "ptz_axis", "camera": "wide", "axis": "zoom"})
        );
        assert_eq!(target.to_string(), "the zoom of PTZ camera wide");
        assert_eq!(target.param(), None);
        let binding: ValueBinding = serde_json::from_value(json!({
            "binding_id": "zoom_knob", "kind": "midi", "controller": 1,
            "from": 0, "to": 16384,
            "target": {"type": "ptz_axis", "camera": "wide", "axis": "zoom"}
        }))
        .expect("parses");
        assert_eq!(binding.problem(), None);
        let unnamed = ValueBinding {
            target: ValueTarget::PtzAxis {
                camera: String::new(),
                axis: PtzAxis::Pan,
            },
            ..binding
        };
        assert_eq!(
            unnamed.problem().map(|p| p.0),
            Some("target.camera".to_owned())
        );
        assert_eq!(PtzAxis::Pan.range(), (-0x7_FFFF, 0x7_FFFF));
        assert_eq!(PtzAxis::Focus.range(), (0, 65535));
    }
}
