use super::*;
use jstream_protocol::{BusId, Edit, MuteTo};

fn note(on: bool, key: u8) -> jstream_midi::Message {
    let status = if on { 0x90 } else { 0x80 };
    jstream_midi::Message::parse(&[status, key, if on { 100 } else { 0 }]).expect("a note")
}

fn cc(channel: u8, controller: u8, value: u8) -> jstream_midi::Message {
    jstream_midi::Message::parse(&[0xB0 | channel, controller, value]).expect("a CC")
}

fn program(number: u8) -> jstream_midi::Message {
    jstream_midi::Message::parse(&[0xC0, number]).expect("a program change")
}

fn talk(bus: &str, to: MuteTo) -> Action {
    Action::BusMute {
        bus_id: BusId::new(bus),
        to,
    }
}

/// Inputs whose MIDI may run bindings: the ones these tests send from.
fn inputs() -> MidiInputs {
    let midi = MidiInputs::new();
    midi.set_bindable(["Pads", "Desk", "Other"].map(String::from));
    midi
}

fn bound(trigger: Trigger, action: Action) -> Graph {
    let mut graph = Graph::new();
    graph
        .apply(&[Edit::BindingSet { trigger, action }], None)
        .expect("binds");
    graph
}

#[test]
fn a_held_note_presses_then_releases_even_between_two_ticks() {
    let graph = bound(
        Trigger::MidiNote {
            note: 36,
            channel: None,
            input: None,
        },
        talk("master", MuteTo::PushToTalk),
    );
    let midi = inputs();
    let mut runner = ActionBindings::default();
    // What was sent before the runner first looked never fires.
    midi.push("Pads", 0, &note(true, 36));
    assert!(runner.tick(&graph, Some(&midi)).is_empty());
    midi.push("Pads", 1, &note(false, 36));
    assert_eq!(
        runner.tick(&graph, Some(&midi)),
        vec![talk("master", MuteTo::Muted)],
        "the note-off releases: muted again"
    );
    // Pressed and let go before the next tick: both run, in order.
    midi.push("Pads", 2, &note(true, 36));
    midi.push("Pads", 3, &note(true, 37));
    midi.push("Pads", 4, &note(false, 36));
    assert_eq!(
        runner.tick(&graph, Some(&midi)),
        vec![
            talk("master", MuteTo::PushToTalk),
            talk("master", MuteTo::Muted)
        ]
    );
}

#[test]
fn a_control_fires_crossing_up_and_releases_falling_back() {
    let graph = bound(
        Trigger::MidiCc {
            controller: 20,
            channel: Some(2),
            input: Some("Desk".into()),
        },
        talk("master", MuteTo::PushToMute),
    );
    let midi = inputs();
    let mut runner = ActionBindings::default();
    assert!(runner.tick(&graph, Some(&midi)).is_empty());
    // Another input, and another channel, are not heard.
    midi.push("Other", 0, &cc(1, 20, 127));
    midi.push("Desk", 0, &cc(0, 20, 127));
    assert!(runner.tick(&graph, Some(&midi)).is_empty());
    midi.push("Desk", 1, &cc(1, 20, 30));
    midi.push("Desk", 2, &cc(1, 20, 100));
    midi.push("Desk", 3, &cc(1, 20, 120));
    assert_eq!(
        runner.tick(&graph, Some(&midi)),
        vec![talk("master", MuteTo::PushToMute)],
        "crossing up fires once; staying above does nothing"
    );
    midi.push("Desk", 4, &cc(1, 20, 0));
    assert_eq!(
        runner.tick(&graph, Some(&midi)),
        vec![talk("master", MuteTo::Unmuted)]
    );
}

#[test]
fn a_program_change_fires_and_has_no_release() {
    let graph = bound(
        Trigger::MidiProgram {
            program: 4,
            channel: None,
            input: None,
        },
        talk("master", MuteTo::Toggle),
    );
    let midi = inputs();
    let mut runner = ActionBindings::default();
    assert!(runner.tick(&graph, Some(&midi)).is_empty());
    midi.push("Desk", 0, &program(3));
    midi.push("Desk", 1, &program(4));
    assert_eq!(
        runner.tick(&graph, Some(&midi)),
        vec![talk("master", MuteTo::Toggle)]
    );
}

#[test]
fn a_toggle_on_a_note_ignores_the_note_off() {
    let graph = bound(
        Trigger::MidiNote {
            note: 40,
            channel: Some(1),
            input: None,
        },
        talk("master", MuteTo::Toggle),
    );
    let midi = inputs();
    let mut runner = ActionBindings::default();
    runner.tick(&graph, Some(&midi));
    midi.push("Pads", 0, &note(true, 40));
    midi.push("Pads", 1, &note(false, 40));
    assert_eq!(
        runner.tick(&graph, Some(&midi)),
        vec![talk("master", MuteTo::Toggle)]
    );
}

/// ADR-0123 §3: an input the person didn't let run bindings (a JTunnel sender with "Use
/// this sender's MIDI" off) is drawn but fires nothing; let in, it fires from then on.
#[test]
fn only_an_input_let_run_bindings_fires_them() {
    let graph = bound(
        Trigger::MidiNote {
            note: 36,
            channel: None,
            input: None,
        },
        talk("master", MuteTo::Toggle),
    );
    let midi = MidiInputs::new();
    let mut runner = ActionBindings::default();
    assert!(runner.tick(&graph, Some(&midi)).is_empty());
    midi.push("Stranger", 0, &note(true, 36));
    assert!(runner.tick(&graph, Some(&midi)).is_empty(), "not let in");
    assert_eq!(midi.any().held(0, 36), 100, "though it is drawn");
    midi.set_bindable(["Stranger".to_owned()]);
    assert!(
        runner.tick(&graph, Some(&midi)).is_empty(),
        "the first look runs nothing"
    );
    midi.push("Stranger", 1, &note(true, 36));
    assert_eq!(
        runner.tick(&graph, Some(&midi)),
        vec![talk("master", MuteTo::Toggle)]
    );
    midi.set_bindable(Vec::new());
    midi.push("Stranger", 2, &note(true, 36));
    assert!(runner.tick(&graph, Some(&midi)).is_empty(), "let out again");
}
