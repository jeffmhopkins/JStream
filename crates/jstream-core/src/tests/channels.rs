//! Channels (ADR-0183): a note or an action drives a channel, `channel.values` reports
//! it, and a value binding's `amount` scales by it.

use super::{batch, block_on, connected, edits, query};
use crate::Engine;
use jstream_control::Control;
use jstream_protocol::ChannelDriver as Driver;
use jstream_protocol::{
    Action, BusId, Call, CallResult, ChannelInfo, ChannelSwitch, ChannelValueInfo, Edit, Finite32,
    MotionCurve, MotionKind, MotionMidiControl, Query, QueryResult, ValueBinding, ValueTarget,
};
use std::time::{Duration, Instant};

fn power() -> Edit {
    let mut power = ChannelInfo::new("power");
    power.inputs = vec![MotionKind::Midi {
        control: MotionMidiControl::Note,
        controller: Some(36),
        channel: None,
        input: None,
        smoothing_ms: 0,
        curve: MotionCurve::Linear,
        from: Finite32::ZERO,
        to: Finite32::ONE,
    }];
    power.off_value = Finite32::new(0.25).expect("finite");
    Edit::ChannelSet { channel: power }
}

fn act(control: &dyn Control, action: Action) {
    match block_on(control.call(Call::Action { action })).map(|r| r.result) {
        Ok(CallResult::Action { .. }) => {}
        other => panic!("not an action result: {other:?}"),
    }
}

fn values(control: &dyn Control) -> Vec<ChannelValueInfo> {
    match query(control, Query::ChannelValues).expect("channel.values") {
        QueryResult::ChannelValues { channels } => channels,
        other => panic!("not channel values: {other:?}"),
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

fn level(control: &dyn Control) -> Option<(f32, bool, Option<Driver>)> {
    values(control)
        .into_iter()
        .find(|c| c.channel_id == "power")
        .map(|c| (c.value.get(), c.on, c.driver))
}

/// A held note drives the channel to 1 and its release back to 0; an action takes over
/// until the input moves again, and switching off holds the off value.
#[test]
fn a_note_and_the_actions_drive_a_channel() {
    let engine = Engine::new();
    let control = connected(&engine);
    batch(&control, edits(vec![power()])).expect("sets");
    assert!(
        until(|| level(&control) == Some((0.0, true, None))),
        "the default until something drives it: {:?}",
        level(&control)
    );
    let midi = engine.midi_inputs();
    // Its MIDI may move things (ADR-0123 §3).
    midi.set_bindable(["Pads"].map(String::from));
    let note = |on: bool| {
        jstream_midi::Message::parse(&[if on { 0x90 } else { 0x80 }, 36, 100]).expect("a note")
    };
    midi.push("Pads", 0, &note(true));
    assert!(
        until(|| level(&control) == Some((1.0, true, Some(Driver::Input { index: 0 })))),
        "held: {:?}",
        level(&control)
    );
    midi.push("Pads", 1, &note(false));
    assert!(
        until(|| level(&control).map(|l| l.0) == Some(0.0)),
        "released: {:?}",
        level(&control)
    );
    act(
        &control,
        Action::ChannelSetValue {
            channel_id: "power".into(),
            value: Finite32::new(0.5).expect("finite"),
            fade_ms: None,
        },
    );
    assert!(
        until(|| level(&control) == Some((0.5, true, Some(Driver::Action)))),
        "set: {:?}",
        level(&control)
    );
    act(
        &control,
        Action::ChannelSwitch {
            channel_id: "power".into(),
            to: ChannelSwitch::Toggle,
        },
    );
    assert!(
        until(|| level(&control).map(|l| (l.0, l.1)) == Some((0.25, false))),
        "off holds the off value: {:?}",
        level(&control)
    );
    let missing = block_on(control.call(Call::Action {
        action: Action::ChannelNudge {
            channel_id: "nowhere".into(),
            by: Finite32::new(0.1).expect("finite"),
            fade_ms: None,
        },
    }));
    assert!(missing.is_err(), "a missing channel: {missing:?}");
}

/// A binding with an `amount` moves from its `from` by the channel's share of the throw.
#[test]
fn an_amount_scales_a_binding_by_its_channel() {
    let engine = Engine::new();
    let control = connected(&engine);
    let binding = Edit::BindingSetValue {
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
            amount: Some("power".into()),
        },
    };
    batch(&control, edits(vec![power(), binding])).expect("sets");
    act(
        &control,
        Action::ChannelSetValue {
            channel_id: "power".into(),
            value: Finite32::new(0.5).expect("finite"),
            fade_ms: None,
        },
    );
    // A binding notes what its control reads on the values thread's first tick after the
    // batch, and drives only once it reads something else. The channel's 0.5 shows in
    // `channel.values` from that tick, so once it does, the CCs below are moves however
    // slow the runner.
    assert!(
        until(|| level(&control).is_some_and(|l| l.0 == 0.5)),
        "the values thread ticks: {:?}",
        level(&control)
    );
    let midi = engine.midi_inputs();
    // Its MIDI may move things (ADR-0123 §3).
    midi.set_bindable(["Bitwig"].map(String::from));
    let cc = |v: u8| jstream_midi::Message::parse(&[0xB0, 7, v]).expect("a CC");
    midi.push("Bitwig", 0, &cc(0));
    midi.push("Bitwig", 1, &cc(127));
    assert!(
        until(|| (engine.drawn_master_gain() + 30.0).abs() < 1e-3),
        "half the throw: {}",
        engine.drawn_master_gain()
    );
}
