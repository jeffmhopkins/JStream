//! Value bindings (ADR-0178): a controller drives a number the collection holds, as the
//! engine draws and hears it, without changing the collection.

use super::{batch, connected, edits, query};
use crate::Engine;
use jstream_protocol::{
    BusId, Edit, Finite32, MotionCurve, MotionKind, MotionMidiControl, Query, QueryResult,
    ValueBinding, ValueTarget,
};
use std::time::{Duration, Instant};

fn master_fader() -> Edit {
    Edit::BindingSetValue {
        binding: ValueBinding {
            binding_id: "master".into(),
            input: MotionKind::Midi {
                control: MotionMidiControl::Cc,
                controller: Some(7),
                channel: None,
                input: None,
                smoothing_ms: 0,
                curve: MotionCurve::Linear,
                from: Finite32::new(-60.0).expect("finite"),
                to: Finite32::ZERO,
            },
            target: ValueTarget::BusGain {
                bus_id: BusId::master(),
            },
            amount: None,
        },
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

fn cc(value: u8) -> jstream_midi::Message {
    jstream_midi::Message::parse(&[0xB0, 7, value]).expect("a CC")
}

/// ADR-0178: a binding waits for its control to move, then its value replaces the
/// fader's as the engine hears it, the collection keeps its own, and clearing the
/// binding gives the collection's back.
#[test]
fn a_fader_drives_the_master_until_its_binding_goes() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(&control, edits(vec![master_fader()])).expect("binds");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        engine.drawn_master_gain(),
        0.0,
        "nothing sent: the stored fader"
    );
    let midi = engine.midi_inputs();
    // Its MIDI may move things (ADR-0123 §3).
    midi.set_bindable(["Bitwig"].map(String::from));
    midi.push("Bitwig", 0, &cc(127));
    std::thread::sleep(Duration::from_millis(100));
    midi.push("Bitwig", 1, &cc(64));
    let half = -60.0 + 60.0 * 64.0 / 127.0;
    assert!(
        until(|| (engine.drawn_master_gain() - half).abs() < 1e-3),
        "the middle of the range: {}",
        engine.drawn_master_gain()
    );
    midi.push("Bitwig", 2, &cc(0));
    assert!(
        until(|| (engine.drawn_master_gain() + 60.0).abs() < 1e-3),
        "the bottom is -60 dB: {}",
        engine.drawn_master_gain()
    );
    let stored = match query(&control, Query::StateGet).expect("state.get") {
        QueryResult::StateGet {
            buses,
            value_bindings,
            ..
        } => {
            assert_eq!(value_bindings.len(), 1, "state.get lists the binding");
            buses
                .iter()
                .find(|bus| bus.bus_id == BusId::master())
                .map(|bus| bus.gain_db.get())
        }
        other => panic!("not a state: {other:?}"),
    };
    assert_eq!(stored, Some(0.0), "the collection keeps its fader");
    batch(
        &control,
        edits(vec![Edit::BindingClearValue {
            binding_id: "master".into(),
        }]),
    )
    .expect("clears");
    assert!(
        until(|| engine.drawn_master_gain() == 0.0),
        "the stored fader again"
    );
}

/// ADR-0123 §3: a JTunnel sender whose "Use this sender's MIDI" is off moves no value
/// binding, whether the binding listens to every input or names the sender; once it is
/// let in, it does.
#[test]
fn a_sender_left_out_moves_no_value_binding() {
    let engine = Engine::new();
    let control = connected(&engine);
    let midi = engine.midi_inputs();
    midi.set_bindable(["Desk".to_owned()]);
    batch(&control, edits(vec![master_fader()])).expect("binds");
    std::thread::sleep(Duration::from_millis(100));
    midi.push("Stranger", 0, &cc(64));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        engine.drawn_master_gain(),
        0.0,
        "every input, the sender left out"
    );
    // A binding that names the sender.
    let mut named = master_fader();
    if let Edit::BindingSetValue {
        binding:
            ValueBinding {
                input: MotionKind::Midi { input, .. },
                ..
            },
    } = &mut named
    {
        *input = Some("Stranger".into());
    }
    batch(&control, edits(vec![named])).expect("rebinds");
    std::thread::sleep(Duration::from_millis(100));
    midi.push("Stranger", 1, &cc(0));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(engine.drawn_master_gain(), 0.0, "its own input, left out");
    // Let in, it drives the fader.
    midi.set_bindable(["Desk", "Stranger"].map(String::from));
    midi.push("Stranger", 2, &cc(32));
    let quarter = -60.0 + 60.0 * 32.0 / 127.0;
    assert!(
        until(|| (engine.drawn_master_gain() - quarter).abs() < 1e-3),
        "let in: {}",
        engine.drawn_master_gain()
    );
}
