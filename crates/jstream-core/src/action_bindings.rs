//! Action bindings as the engine runs them
//! ([ADR-0122](../../../docs/adr/0122-controller-input-midi-and-one-binding-system.md)
//! decision 1, [ADR-0207](../../../docs/adr/0207-mute-actions-and-held-triggers.md)):
//! a `binding.set` trigger heard on a MIDI input runs its action, and its release runs
//! the action's [`Action::on_release`].
//!
//! The `jstream-values` thread ticks the runner about 30 times a second (60 while a
//! value moves, ADR-0190) while the collection holds a binding. Each tick reads, from each input a binding listens to
//! (each input, for a binding with no `input`), the notes and the control and program
//! changes logged since the last tick, in the order they arrived, so a pad pressed and
//! let go between two ticks still fires both. Nothing heard before the first tick that
//! follows the input fires. Only an input [`MidiInputs::bindable`] names is read: a
//! JTunnel sender's MIDI runs bindings only once the person turns on "Use this
//! sender's MIDI" ([ADR-0123](../../../docs/adr/0123-lan-inputs-jtunnel-and-visca.md)
//! §3).
//!
//! - A note binding fires on its note-on and releases on its note-off (or a note-on
//!   at velocity 0).
//! - A control change binding fires when the controller crosses the middle (64)
//!   upward, and releases when it falls back below it, each on its own channel. A
//!   controller not heard before is taken to rest at 0.
//! - A program change binding fires on its program change, and has no release.
//!
//! A press whose action has a release is held until its release is heard, and is let
//! go (its release runs) sooner if what it came from goes away: its input may no longer
//! run bindings, its binding is changed or removed, the MIDI inputs go, or its JTunnel
//! sender has not been heard from for [`SILENT_MS`]. So a push-to-talk never stays open
//! because its note-off was lost.
//!
//! Docs: docs/adr/0207-mute-actions-and-held-triggers.md

use std::collections::BTreeMap;

use jstream_graph::Graph;
use jstream_protocol::{Action, Trigger};
use jstream_sources::{ControlEvent, ControlKind, MidiInputs, NoteEvent};

/// The value a control change crosses to fire: the middle of 0 to 127.
const MIDDLE: u8 = 64;

/// How long a JTunnel sender may go unheard before its held presses are let go, in
/// milliseconds. A sender sends a frame every few milliseconds while linked, even with
/// no MIDI, so this is only reached once it has dropped.
pub(crate) const SILENT_MS: u64 = 500;

/// One event from an input's logs, notes and controls together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Heard {
    /// A note-on, or a note-off at velocity 0.
    Note(NoteEvent),
    /// A control change, with the value the controller had before it on that channel.
    Change { event: ControlEvent, before: u8 },
    /// A program change.
    Program(ControlEvent),
}

impl Heard {
    fn at_ms(&self) -> u64 {
        match self {
            Heard::Note(event) => event.at_ms,
            Heard::Change { event, .. } | Heard::Program(event) => event.at_ms,
        }
    }
}

/// What a heard event does to a binding's trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fired {
    /// Runs its action.
    Press,
    /// Runs its action's release, if it has one.
    Release,
}

/// What `heard` does to `trigger`, or `None` when it doesn't match. The input was
/// matched already: `heard` came from the input the trigger listens to.
pub(crate) fn fired(trigger: &Trigger, heard: &Heard) -> Option<Fired> {
    // A trigger's channel is 1 to 16; an event's 0 to 15.
    let on_channel = |channel: u8| trigger.channel().is_none_or(|c| c == channel + 1);
    match (trigger, heard) {
        (Trigger::MidiNote { note, .. }, Heard::Note(event))
            if event.key == *note && on_channel(event.channel) =>
        {
            Some(if event.velocity > 0 {
                Fired::Press
            } else {
                Fired::Release
            })
        }
        (
            Trigger::MidiCc { controller, .. },
            Heard::Change {
                event:
                    ControlEvent {
                        channel,
                        kind:
                            ControlKind::Change {
                                controller: heard,
                                value,
                            },
                        ..
                    },
                before,
            },
        ) if heard == controller && on_channel(*channel) => {
            match (*before >= MIDDLE, *value >= MIDDLE) {
                (false, true) => Some(Fired::Press),
                (true, false) => Some(Fired::Release),
                _ => None,
            }
        }
        (
            Trigger::MidiProgram { program, .. },
            Heard::Program(ControlEvent {
                channel,
                kind: ControlKind::Program(heard),
                ..
            }),
        ) if heard == program && on_channel(*channel) => Some(Fired::Press),
        _ => None,
    }
}

/// What the runner has read of one input's logs.
#[derive(Debug, Default)]
struct Seen {
    notes: u64,
    controls: u64,
    /// Each controller's last value, by channel and controller.
    values: BTreeMap<(u8, u8), u8>,
}

/// A press whose release has not been heard yet.
#[derive(Debug)]
struct Held {
    input: String,
    trigger: Trigger,
    /// The action it ran, which its binding must still run for it to stay held.
    action: Action,
    /// What letting it go runs.
    release: Action,
}

/// The action bindings between ticks: how far each input's logs have been read, and
/// the presses held.
#[derive(Debug, Default)]
pub(crate) struct ActionBindings {
    /// By input's name.
    seen: BTreeMap<String, Seen>,
    held: Vec<Held>,
    notes: Vec<NoteEvent>,
    controls: Vec<ControlEvent>,
}

impl ActionBindings {
    /// Reads what each input `graph`'s bindings listen to has sent since the last tick,
    /// and returns the actions to run, in the order their triggers were heard.
    pub(crate) fn tick(&mut self, graph: &Graph, midi: Option<&MidiInputs>) -> Vec<Action> {
        let Some(midi) = midi else {
            self.seen.clear();
            return self.held.drain(..).map(|held| held.release).collect();
        };
        // Each input that may run bindings, with the bindings listening to it: those
        // naming it and those listening to every input. Only an input whose MIDI the
        // person let run bindings is read (ADR-0123 §3).
        let mut inputs: BTreeMap<String, Vec<(&Trigger, &Action)>> = BTreeMap::new();
        let bindable = midi.bindable();
        for (trigger, action) in graph.state().bindings() {
            for name in &bindable {
                if trigger.input().is_none_or(|input| input == name) {
                    inputs
                        .entry(name.clone())
                        .or_default()
                        .push((trigger, action));
                }
            }
        }
        // An input no binding listens to any more is forgotten, so a binding made later
        // starts from what it sends then.
        self.seen.retain(|input, _| inputs.contains_key(input));
        // A held press is let go once its input may no longer run bindings, its binding
        // no longer runs its action, or its sender fell silent.
        let now_ms = midi.now_ms();
        let mut run = Vec::new();
        self.held.retain(|held| {
            let bound = inputs.get(&held.input).is_some_and(|bindings| {
                bindings
                    .iter()
                    .any(|(trigger, action)| **trigger == held.trigger && **action == held.action)
            });
            let silent = midi
                .input(&held.input)
                .and_then(|state| state.alive_ms())
                .zip(now_ms)
                .is_some_and(|(alive, now)| now.saturating_sub(alive) > SILENT_MS);
            if bound && !silent {
                return true;
            }
            run.push(held.release.clone());
            false
        });
        for (input, bindings) in inputs {
            let Some(state) = midi.input(&input) else {
                continue;
            };
            let Some(seen) = self.seen.get_mut(&input) else {
                self.seen.insert(
                    input,
                    Seen {
                        notes: state.pushed(),
                        controls: state.controls_pushed(),
                        values: BTreeMap::new(),
                    },
                );
                continue;
            };
            self.notes.clear();
            self.controls.clear();
            seen.notes = state.notes_since(seen.notes, &mut self.notes);
            seen.controls = state.controls_since(seen.controls, &mut self.controls);
            let mut heard: Vec<Heard> = self.notes.iter().copied().map(Heard::Note).collect();
            for event in &self.controls {
                heard.push(match event.kind {
                    ControlKind::Change { controller, value } => {
                        let before = seen
                            .values
                            .insert((event.channel, controller), value)
                            .unwrap_or(0);
                        Heard::Change {
                            event: *event,
                            before,
                        }
                    }
                    ControlKind::Program(_) => Heard::Program(*event),
                });
            }
            // Stable, so events at the same millisecond keep the order they came in.
            heard.sort_by_key(Heard::at_ms);
            for event in &heard {
                for &(trigger, action) in &bindings {
                    match fired(trigger, event) {
                        Some(Fired::Press) => {
                            run.push(action.clone());
                            if let Some(release) = action.on_release() {
                                self.held
                                    .retain(|held| held.input != input || held.trigger != *trigger);
                                self.held.push(Held {
                                    input: input.clone(),
                                    trigger: trigger.clone(),
                                    action: action.clone(),
                                    release,
                                });
                            }
                        }
                        Some(Fired::Release) => {
                            self.held
                                .retain(|held| held.input != input || held.trigger != *trigger);
                            run.extend(action.on_release());
                        }
                        None => {}
                    }
                }
            }
        }
        run
    }
}

#[cfg(test)]
mod tests;
