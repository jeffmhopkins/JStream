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
}
