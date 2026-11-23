//! `source.mute` and `bus.mute` (ADR-0207): they set a mute as `source.set_audio` and
//! `bus.set_fader` do, with a `rev` and no undo step; a hotkey or MIDI binding runs them,
//! and a held note's release runs `push_to_talk`'s and `push_to_mute`'s opposite.

use super::{batch, block_on, code, connected, edits, query};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::{
    Action, BusId, Call, Chord, Edit, HistoryCall, HistoryOp, MuteTo, Query, QueryResult, Settings,
    SourceId, SourceKind, Trigger,
};
use std::time::{Duration, Instant};

fn act(control: &dyn Control, action: Action) -> Result<(), &'static str> {
    block_on(control.call(Call::Action { action }))
        .map(|_| ())
        .map_err(|error| code(&error).unwrap_or("not a protocol error"))
}

fn bus(id: &str, to: MuteTo) -> Action {
    Action::BusMute {
        bus_id: BusId::new(id),
        to,
    }
}

fn source(id: &str, to: MuteTo) -> Action {
    Action::SourceMute {
        source_id: SourceId::new(id),
        to,
    }
}

fn create(id: &str, kind: &str) -> Edit {
    Edit::SourceCreate {
        source_id: SourceId::new(id),
        name: id.into(),
        kind: SourceKind::Builtin { kind: kind.into() },
        settings: Settings::default(),
        duplicated_from: None,
    }
}

/// The collection's `rev`, the master's mute, and source `mic`'s, if it has audio.
fn muted(control: &dyn Control) -> (u64, bool, Option<bool>) {
    match query(control, Query::StateGet).expect("state.get") {
        QueryResult::StateGet {
            rev,
            buses,
            sources,
            ..
        } => (
            rev.0,
            buses
                .iter()
                .find(|bus| bus.bus_id == BusId::master())
                .is_some_and(|bus| bus.muted),
            sources
                .iter()
                .find(|s| s.source_id.as_str() == "mic")
                .and_then(|s| s.audio.map(|audio| audio.muted)),
        ),
        other => panic!("not a state: {other:?}"),
    }
}

/// Waits up to two seconds for `done`.
fn until(done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    done()
}

#[test]
fn mute_actions_set_the_mute_with_a_rev_and_no_undo_step() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(&control, edits(vec![create("mic", "audio_input")])).expect("creates");
    let (rev, master, mic) = muted(&control);
    assert_eq!((master, mic), (false, Some(false)));
    act(&control, bus("master", MuteTo::Muted)).expect("mutes");
    assert_eq!(muted(&control), (rev + 1, true, Some(false)));
    // Already muted: nothing changes, not even the rev.
    act(&control, bus("master", MuteTo::Muted)).expect("succeeds");
    act(&control, bus("master", MuteTo::PushToMute)).expect("succeeds");
    assert_eq!(muted(&control).0, rev + 1);
    act(&control, bus("master", MuteTo::Toggle)).expect("toggles");
    act(&control, source("mic", MuteTo::Toggle)).expect("toggles");
    assert_eq!(muted(&control), (rev + 3, false, Some(true)));
    act(&control, source("mic", MuteTo::PushToTalk)).expect("the press unmutes");
    assert_eq!(muted(&control).2, Some(false));
    // An action is never an undo step: undo takes back the source's creation.
    block_on(control.call(Call::History(HistoryCall::new(HistoryOp::Undo)))).expect("undo");
    assert_eq!(muted(&control).2, None, "the create was the last undo step");
}

#[test]
fn mute_actions_refuse_what_has_no_mute() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(&control, edits(vec![create("red", "color")])).expect("creates");
    assert_eq!(act(&control, bus("aux", MuteTo::Muted)), Err("not_found"));
    assert_eq!(
        act(&control, source("nope", MuteTo::Muted)),
        Err("not_found")
    );
    assert_eq!(
        act(&control, source("red", MuteTo::Muted)),
        Err("invalid_argument"),
        "a source without audio has no mute"
    );
}

#[test]
fn a_mute_binding_goes_with_its_bus() {
    let engine = Engine::new();
    let control = connected(&engine);
    let chord = Chord::new("Ctrl+KeyM");
    batch(
        &control,
        edits(vec![
            Edit::BusCreate {
                bus_id: BusId::new("aux"),
                name: "Aux".into(),
            },
            Edit::HotkeySet {
                chord: chord.clone(),
                action: bus("aux", MuteTo::PushToTalk),
            },
        ]),
    )
    .expect("binds");
    let hotkeys = |control: &dyn Control| match query(control, Query::StateGet) {
        Ok(QueryResult::StateGet { hotkeys, .. }) => hotkeys.len(),
        other => panic!("not a state: {other:?}"),
    };
    assert_eq!(hotkeys(&control), 1);
    batch(
        &control,
        edits(vec![Edit::BusDelete {
            bus_id: BusId::new("aux"),
        }]),
    )
    .expect("deletes");
    assert_eq!(hotkeys(&control), 0, "the binding named the bus");
    let unknown = Edit::HotkeySet {
        chord,
        action: bus("aux", MuteTo::Muted),
    };
    assert!(batch(&control, edits(vec![unknown])).is_err());
}

/// A pad bound to push-to-talk: the note-on unmutes the master, the note-off mutes it
/// again, run by the engine itself (ADR-0122 decision 1, ADR-0207), from an input the
/// person let run bindings.
#[test]
fn a_held_pad_talks_until_it_is_let_go() {
    let engine = Engine::new();
    let control = connected(&engine);
    act(&control, bus("master", MuteTo::Muted)).expect("mutes");
    batch(
        &control,
        edits(vec![Edit::BindingSet {
            trigger: Trigger::MidiNote {
                note: 36,
                channel: None,
                input: None,
            },
            action: bus("master", MuteTo::PushToTalk),
        }]),
    )
    .expect("binds");
    // A JTunnel sender with "Use this sender's MIDI" on (ADR-0123 §3).
    let midi = engine.midi_inputs();
    midi.set_bindable(["Pads".to_owned()]);
    // The runner's first tick only looks.
    std::thread::sleep(Duration::from_millis(100));
    let note = |on: bool| {
        jstream_midi::Message::parse(&[if on { 0x90 } else { 0x80 }, 36, 100]).expect("a note")
    };
    midi.push("Pads", 0, &note(true));
    assert!(until(|| !muted(&control).1), "held: the master talks");
    midi.push("Pads", 1, &note(false));
    assert!(until(|| muted(&control).1), "let go: muted again");
}
