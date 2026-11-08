use super::*;
use crate::{Registry, Source};
use serde_json::{Value, json};

fn ms(ms: u64) -> MediaTime {
    at_millis(ms)
}

/// A message from its bytes, as a port delivers it.
fn msg(bytes: &[u8]) -> Message {
    Message::parse(bytes).expect("a MIDI message")
}

/// Note on, channel 1.
fn on(key: u8, velocity: u8) -> Message {
    msg(&[0x90, key, velocity])
}

/// Note off, channel 1.
fn off(key: u8) -> Message {
    msg(&[0x80, key, 0])
}

/// The defaults of `kind` with `overrides` on them.
fn settings(kind: &str, overrides: Value) -> Settings {
    let mut settings: Settings = Registry::builtin()
        .descriptor(kind)
        .expect("a MIDI kind")
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    if let Value::Object(map) = overrides {
        for (k, v) in map {
            settings.insert(k, v);
        }
    }
    settings
}

/// A source of `kind` drawing `inputs`.
fn source(kind: &str, inputs: &Arc<MidiInputs>, overrides: Value) -> Box<dyn Source> {
    let ctx = Context {
        midi: Some(Arc::clone(inputs)),
        ..Context::default()
    };
    Registry::builtin()
        .create(kind, &settings(kind, overrides), &ctx)
        .expect("a MIDI source")
}

/// Paints `source`'s frame for `at`, as a host does off its locks, and takes it.
fn draw(source: &mut Box<dyn Source>, at: MediaTime) -> Arc<VideoFrame> {
    if let Some(painter) = source.painter() {
        painter.paint(at);
    }
    source.frame(at).expect("a frame")
}

fn pixel(frame: &VideoFrame, x: u32, y: u32) -> [u8; 4] {
    let at = x as usize * 4;
    let row = frame.row(0, y).expect("a row");
    [row[at], row[at + 1], row[at + 2], row[at + 3]]
}

const RED: [u8; 4] = [0xff, 0, 0, 0xff];
const WHITE: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
const BLACK: [u8; 4] = [0, 0, 0, 0xff];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

#[test]
fn a_note_event_packs_into_one_word_and_back() {
    let event = NoteEvent {
        at_ms: (1 << 40) - 1,
        channel: 15,
        key: 127,
        velocity: 127,
    };
    assert_eq!(NoteEvent::unpack(event.pack()), event);
    let event = NoteEvent {
        at_ms: 1234,
        channel: 9,
        key: 36,
        velocity: 0,
    };
    assert_eq!(NoteEvent::unpack(event.pack()), event);
}

#[test]
fn the_state_holds_keys_controllers_bend_and_pressure_by_channel() {
    let state = MidiState::new();
    assert_eq!(state.last_ms(), None);
    state.push(5, &on(60, 100));
    state.push(6, &msg(&[0x99, 36, 90])); // channel 10
    state.push(7, &msg(&[0xB0, 7, 64]));
    state.push(8, &msg(&[0xE0, 0x00, 0x60])); // bend up
    state.push(9, &msg(&[0xD1, 33])); // channel 2 pressure
    assert_eq!(state.held(0, 60), 100);
    assert_eq!(state.held(9, 36), 90);
    assert_eq!(state.held(0, 36), 0, "keys are per channel");
    assert_eq!(state.controller(0, 7), 64);
    assert!(state.bend(0) > 0);
    assert_eq!(state.pressure(1), 33);
    assert_eq!(state.last_ms(), Some(9));
    assert_eq!(state.pushed(), 2, "only notes are logged");

    // A note-on at velocity 0 is a note-off; so is a note-off.
    state.push(10, &on(60, 0));
    assert_eq!(state.held(0, 60), 0);
    state.push(11, &on(61, 1));
    state.push(12, &off(61));
    assert_eq!(state.held(0, 61), 0);

    // All notes off releases every key on its channel, and logs each release.
    state.push(13, &on(64, 80));
    state.push(13, &on(67, 80));
    let before = state.pushed();
    state.push(14, &msg(&[0xB0, 123, 0]));
    assert_eq!((state.held(0, 64), state.held(0, 67)), (0, 0));
    assert_eq!(state.pushed(), before + 2);
    assert_eq!(state.held(9, 36), 90, "another channel keeps its keys");
}

#[test]
fn the_log_reads_the_latest_events_oldest_first() {
    let state = MidiState::new();
    let mut out = Vec::with_capacity(LOG);
    state.recent(&mut out, LOG);
    assert!(out.is_empty());
    for n in 0..(LOG as u64 + 10) {
        state.push(n, &on((n % 128) as u8, 1 + (n % 127) as u8));
    }
    state.recent(&mut out, LOG);
    assert_eq!(out.len(), LOG);
    assert_eq!(out[0].at_ms, 10, "the oldest ten were overwritten");
    assert_eq!(out[LOG - 1].at_ms, LOG as u64 + 9);
    assert!(out.windows(2).all(|w| w[0].at_ms < w[1].at_ms));
    state.recent(&mut out, 3);
    assert_eq!(
        out.iter().map(|e| e.at_ms).collect::<Vec<_>>(),
        [LOG as u64 + 7, LOG as u64 + 8, LOG as u64 + 9]
    );
}

#[test]
fn a_named_input_feeds_itself_and_every_input_state() {
    let inputs = MidiInputs::new();
    inputs.push("Keys", 1, &on(60, 100));
    inputs.push("Pads", 2, &on(36, 90));
    let keys = inputs.drawn("Keys");
    assert_eq!(keys.held(0, 60), 100);
    assert_eq!(keys.held(0, 36), 0);
    let any = inputs.drawn("");
    assert_eq!((any.held(0, 60), any.held(0, 36)), (100, 90));
    assert!(
        Arc::ptr_eq(&keys, &inputs.drawn("Keys")),
        "one state per name"
    );

    // Names past the limits get a state that never fills.
    assert!(inputs.input("").is_none());
    assert!(inputs.input(&"x".repeat(MAX_INPUT_NAME + 1)).is_none());
    for n in inputs.named_len()..MAX_INPUTS {
        assert!(inputs.input(&format!("in {n}")).is_some());
    }
    assert!(inputs.input("one too many").is_none());
    assert!(
        inputs.input("Keys").is_some(),
        "a known name still resolves"
    );
    inputs.push("one too many", 3, &on(62, 50));
    assert_eq!(
        inputs.any().held(0, 62),
        50,
        "its messages still reach every input"
    );
}

impl MidiInputs {
    fn named_len(&self) -> usize {
        lock(&self.named).len()
    }
}

#[test]
fn every_midi_kind_validates_its_input_channel_and_size() {
    let registry = Registry::builtin();
    let ctx = Context::default();
    for kind in ["midi_keyboard", "midi_piano_roll", "midi_pads", "midi_cc"] {
        registry
            .create(kind, &settings(kind, json!({})), &ctx)
            .unwrap_or_else(|e| panic!("{kind}'s defaults: {e}"));
        for (bad, param) in [
            (json!({"channel": 17}), "channel"),
            (json!({"input": "a\nb"}), "input"),
            (json!({"input": "x".repeat(MAX_INPUT_NAME + 1)}), "input"),
            (json!({"width": 0}), "width"),
        ] {
            match registry.create(kind, &settings(kind, bad.clone()), &ctx) {
                Err(SourceError::Setting { param: p, .. }) => assert_eq!(p, param, "{kind}"),
                Err(e) => panic!("{kind} with {bad}: {e}"),
                Ok(_) => panic!("{kind} took {bad}"),
            }
        }
    }
}

#[test]
fn without_midi_a_source_draws_its_first_frame_once() {
    let mut keyboard = Registry::builtin()
        .create(
            "midi_keyboard",
            &settings(
                "midi_keyboard",
                json!({"width": 20, "height": 10, "low_key": 60, "high_key": 62}),
            ),
            &Context::default(),
        )
        .expect("a source");
    let frame = draw(&mut keyboard, ms(0));
    assert_eq!(keyboard.next_change(ms(0)), None);
    assert_eq!(
        pixel(&frame, 2, 9),
        [0xf2, 0xf2, 0xf2, 0xff],
        "an up white key"
    );
}

/// A keyboard of middle C, C sharp and D: two white keys ten pixels wide, and one
/// black key six wide over the line between them.
fn keyboard(inputs: &Arc<MidiInputs>, overrides: Value) -> Box<dyn Source> {
    let mut base = json!({
        "width": 20, "height": 10, "low_key": 60, "high_key": 62,
        "color": "#ff0000", "white_key": "#ffffff", "black_key": "#000000",
    });
    if let (Value::Object(base), Value::Object(more)) = (&mut base, overrides) {
        base.extend(more);
    }
    source("midi_keyboard", inputs, base)
}

#[test]
fn the_keyboard_lights_held_keys_by_velocity() {
    let inputs = Arc::new(MidiInputs::new());
    let mut keys = keyboard(&inputs, json!({}));
    let frame = draw(&mut keys, ms(0));
    assert_eq!(pixel(&frame, 2, 9), WHITE);
    assert_eq!(pixel(&frame, 15, 9), WHITE);
    assert_eq!(pixel(&frame, 10, 2), BLACK, "C sharp sits over the line");
    assert_eq!(pixel(&frame, 9, 9), CLEAR, "a gap between white keys");

    inputs.push("Keys", 10, &on(62, 127));
    inputs.push("Keys", 10, &on(61, 1));
    let frame = draw(&mut keys, ms(40));
    assert_eq!(pixel(&frame, 15, 9), RED, "D at full velocity");
    assert_eq!(pixel(&frame, 2, 9), WHITE);
    let soft = pixel(&frame, 10, 2);
    assert!(
        soft[0] > 80 && soft[0] < 100 && soft[1] == 0,
        "C sharp at 35%: {soft:?}"
    );

    inputs.push("Keys", 50, &off(62));
    let frame = draw(&mut keys, ms(70));
    assert_eq!(pixel(&frame, 15, 9), WHITE, "released");
}

#[test]
fn the_keyboard_draws_only_its_input_and_channel() {
    let inputs = Arc::new(MidiInputs::new());
    let mut keys = keyboard(&inputs, json!({"input": "Keys", "channel": 2}));
    inputs.push("Other", 0, &msg(&[0x91, 62, 127]));
    inputs.push("Keys", 0, &on(62, 127));
    assert_eq!(pixel(&draw(&mut keys, ms(0)), 15, 9), WHITE);
    inputs.push("Keys", 40, &msg(&[0x91, 62, 127]));
    assert_eq!(pixel(&draw(&mut keys, ms(40)), 15, 9), RED);
}

#[test]
fn a_still_input_settles_on_one_frame_and_polls_slowly() {
    let inputs = Arc::new(MidiInputs::new());
    let mut keys = keyboard(&inputs, json!({}));
    let first = draw(&mut keys, ms(0));
    assert_eq!(keys.next_change(ms(0)), Some(ms(34)));
    let again = draw(&mut keys, ms(34));
    assert!(
        Arc::ptr_eq(&first, &again),
        "nothing changed: the same frame"
    );
    assert_eq!(keys.next_change(ms(34)), Some(ms(284)));

    // A note redraws at the next tick.
    inputs.push("", 300, &on(60, 127));
    let lit = draw(&mut keys, ms(300));
    assert!(!Arc::ptr_eq(&first, &lit));
    assert_eq!(pixel(&lit, 2, 9), RED);

    // A live change redraws too; a size change needs a new source.
    let mut changed = settings(
        "midi_keyboard",
        json!({"width": 20, "height": 10, "low_key": 60, "high_key": 62,
               "color": "#0000ff", "white_key": "#ffffff", "black_key": "#000000"}),
    );
    keys.update(&changed).expect("a live update");
    assert_eq!(pixel(&draw(&mut keys, ms(334)), 2, 9), [0, 0, 0xff, 0xff]);
    changed.insert("width", json!(30));
    assert!(matches!(
        keys.update(&changed),
        Err(SourceError::NotLive { .. })
    ));
}

#[test]
fn a_key_range_must_run_low_to_high() {
    let err = Registry::builtin()
        .create(
            "midi_piano_roll",
            &settings("midi_piano_roll", json!({"low_key": 70, "high_key": 60})),
            &Context::default(),
        )
        .err()
        .expect("refused");
    assert!(matches!(err, SourceError::Setting { ref param, .. } if param == "high_key"));
}

#[test]
fn a_pad_lights_on_its_hit_and_fades_over_the_decay() {
    let inputs = Arc::new(MidiInputs::new());
    // Two pads side by side, keys 36 and 37, on channel 10.
    let mut pads = source(
        "midi_pads",
        &inputs,
        json!({"width": 20, "height": 10, "rows": 1, "columns": 2, "decay_ms": 100,
               "color": "#ff0000", "pad": "#000000"}),
    );
    assert_eq!(pixel(&draw(&mut pads, ms(0)), 15, 5), BLACK);
    // A short hit: on and off at once.
    inputs.push("", 40, &msg(&[0x99, 37, 127]));
    inputs.push("", 40, &msg(&[0x89, 37, 0]));
    // On channel 1 it is not drawn.
    inputs.push("", 40, &on(36, 127));
    let frame = draw(&mut pads, ms(40));
    assert_eq!(pixel(&frame, 15, 5), RED, "lit at once");
    assert_eq!(pixel(&frame, 5, 5), BLACK);
    let fading = pixel(&draw(&mut pads, ms(90)), 15, 5);
    assert!(fading[0] < 0xff && fading[0] > 0, "half way: {fading:?}");
    assert_eq!(pads.next_change(ms(90)), Some(ms(100)), "still moving");
    assert_eq!(pixel(&draw(&mut pads, ms(200)), 15, 5), BLACK, "faded");
    let settled = draw(&mut pads, ms(240));
    assert!(Arc::ptr_eq(&settled, &draw(&mut pads, ms(280))));

    // A held note stays lit past the decay.
    inputs.push("", 300, &msg(&[0x99, 36, 127]));
    assert_eq!(pixel(&draw(&mut pads, ms(1000)), 5, 5), RED);
}

#[test]
fn the_cc_meter_draws_each_controller_and_bend() {
    let inputs = Arc::new(MidiInputs::new());
    let mut cc = source(
        "midi_cc",
        &inputs,
        json!({"width": 32, "height": 10, "controllers": "7, 74", "pitch_bend": true,
               "color": "#ff0000", "track": "#000000"}),
    );
    // Three bars, each about ten pixels wide, centred near 5, 16 and 26.
    let frame = draw(&mut cc, ms(0));
    assert_eq!(pixel(&frame, 5, 9), BLACK);
    assert_eq!(
        pixel(&frame, 26, 5),
        RED,
        "a bend of 0 is a line at the middle"
    );
    assert_eq!(pixel(&frame, 26, 2), BLACK);

    inputs.push("", 10, &msg(&[0xB0, 7, 127]));
    inputs.push("", 10, &msg(&[0xB0, 74, 64]));
    inputs.push("", 10, &msg(&[0xE0, 0x7F, 0x7F])); // full bend up
    let frame = draw(&mut cc, ms(40));
    assert_eq!(
        (pixel(&frame, 5, 0), pixel(&frame, 5, 9)),
        (RED, RED),
        "full"
    );
    assert_eq!(pixel(&frame, 16, 9), RED);
    assert_eq!(pixel(&frame, 16, 1), BLACK, "half way");
    assert_eq!(
        (pixel(&frame, 26, 1), pixel(&frame, 26, 8)),
        (RED, BLACK),
        "up"
    );
}

#[test]
fn the_controller_list_must_be_numbers_and_at_most_sixteen() {
    for bad in [
        "1, x",
        "128",
        "-1",
        &(0..17).map(|n| n.to_string()).collect::<Vec<_>>().join(","),
    ] {
        let err = Registry::builtin()
            .create(
                "midi_cc",
                &settings("midi_cc", json!({"controllers": bad})),
                &Context::default(),
            )
            .err()
            .unwrap_or_else(|| panic!("took {bad:?}"));
        assert!(matches!(err, SourceError::Setting { ref param, .. } if param == "controllers"));
    }
    Registry::builtin()
        .create(
            "midi_cc",
            &settings("midi_cc", json!({"controllers": ""})),
            &Context::default(),
        )
        .expect("an empty list draws nothing");
}

#[test]
fn the_piano_roll_scrolls_notes_left_from_the_right_edge() {
    let inputs = Arc::new(MidiInputs::new());
    // One lane, a second wide over 100 pixels: 10 ms a pixel.
    let mut roll = source(
        "midi_piano_roll",
        &inputs,
        json!({"width": 100, "height": 10, "low_key": 60, "high_key": 61, "seconds": 1.0,
               "color": "#ff0000"}),
    );
    inputs.push("", 0, &on(60, 127));
    inputs.push("", 500, &off(60));
    inputs.push("", 800, &on(61, 127));
    let frame = draw(&mut roll, ms(1000));
    // Key 60 is the bottom lane; it sounded from 0 to 500 ms: the left half.
    assert_eq!(pixel(&frame, 25, 8), RED);
    assert_eq!(pixel(&frame, 75, 8), CLEAR);
    // Key 61 still sounds, from 800 ms to the right edge.
    assert_eq!(pixel(&frame, 90, 2), RED);
    assert_eq!(pixel(&frame, 70, 2), CLEAR);
    assert_eq!(roll.next_change(ms(1000)), Some(ms(1034)), "it scrolls");

    // Half a second later key 60 has scrolled off, and 61 runs from the left at 30.
    let frame = draw(&mut roll, ms(1500));
    assert_eq!(pixel(&frame, 10, 8), CLEAR);
    assert_eq!(pixel(&frame, 20, 2), CLEAR);
    assert_eq!(pixel(&frame, 40, 2), RED);
}

#[test]
fn the_piano_roll_draws_a_held_key_the_log_has_lost() {
    let inputs = Arc::new(MidiInputs::new());
    let mut roll = source(
        "midi_piano_roll",
        &inputs,
        json!({"width": 100, "height": 10, "low_key": 60, "high_key": 60, "seconds": 1.0,
               "color": "#ff0000"}),
    );
    inputs.push("", 0, &on(60, 127));
    // Enough other notes to push its note-on out of the log.
    for n in 0..LOG as u64 {
        inputs.push("", 1 + n / 1000, &on(70, 1));
    }
    let frame = draw(&mut roll, ms(5000));
    assert_eq!(pixel(&frame, 0, 5), RED, "held from the left edge");
    assert_eq!(pixel(&frame, 99, 5), RED);
}

/// Review #1374 B1: a settled source redraws for any message, even one that moves no
/// clock and logs no note, such as a second controller change in the same millisecond.
#[test]
fn a_settled_source_redraws_for_a_controller_change_in_the_same_millisecond() {
    let inputs = Arc::new(MidiInputs::new());
    let mut cc = source(
        "midi_cc",
        &inputs,
        json!({"width": 10, "height": 10, "controllers": "7",
               "color": "#ff0000", "track": "#000000"}),
    );
    inputs.push("", 100, &msg(&[0xB0, 7, 127]));
    assert_eq!(pixel(&draw(&mut cc, ms(100)), 5, 0), RED);
    // Settled on that frame...
    draw(&mut cc, ms(134));
    // ...a second change at the same millisecond, and one from an input whose clock
    // lags, both still show.
    inputs.push("", 100, &msg(&[0xB0, 7, 0]));
    assert_eq!(pixel(&draw(&mut cc, ms(167)), 5, 0), BLACK);
    draw(&mut cc, ms(200));
    inputs.push("Late", 50, &msg(&[0xB0, 7, 127]));
    assert_eq!(pixel(&draw(&mut cc, ms(234)), 5, 0), RED);
}

/// Review #1374 N6: a live channel change drops the old channel's fading hits.
#[test]
fn a_live_channel_change_drops_the_old_channels_hits() {
    let inputs = Arc::new(MidiInputs::new());
    let overrides = json!({"width": 20, "height": 10, "rows": 1, "columns": 2,
                           "decay_ms": 1000, "color": "#ff0000", "pad": "#000000"});
    let mut pads = source("midi_pads", &inputs, overrides.clone());
    inputs.push("", 40, &msg(&[0x99, 36, 127]));
    inputs.push("", 40, &msg(&[0x89, 36, 0]));
    assert_eq!(pixel(&draw(&mut pads, ms(40)), 5, 5), RED);
    let mut changed = settings("midi_pads", overrides);
    changed.insert("channel", json!(1));
    pads.update(&changed).expect("a live update");
    assert_eq!(pixel(&draw(&mut pads, ms(100)), 5, 5), BLACK);
}

#[test]
fn times_at_the_end_of_the_clock_saturate() {
    let state = MidiState::new();
    state.push(u64::MAX, &on(60, 1));
    assert_eq!(state.last_ms(), Some(u64::MAX - 1));
    assert!(tick(at_millis(u64::MAX / 2)) > 0);
}

/// A source made before the host sets the clock draws against it once set: a note's age
/// is measured on the media clock it was stamped on, not on the time since the source
/// started, which a painter is given.
#[test]
fn a_source_measures_note_ages_on_the_inputs_clock() {
    struct At(u64);
    impl MediaClock for At {
        fn timebase(&self) -> Timebase {
            Timebase::MILLIS
        }
        fn now(&self) -> MediaTime {
            at_millis(self.0)
        }
    }
    let inputs = Arc::new(MidiInputs::new());
    let mut pads = source(
        "midi_pads",
        &inputs,
        json!({"width": 20, "height": 10, "rows": 1, "columns": 2, "decay_ms": 100,
               "color": "#ff0000", "pad": "#000000"}),
    );
    assert!(inputs.set_clock(Arc::new(At(10_000))));
    assert!(
        !inputs.set_clock(Arc::new(At(0))),
        "only the first clock counts"
    );
    assert_eq!(inputs.now_ms(), Some(10_000));
    // Painted at the source's 40 ms: a hit 10 ms ago on the media clock still shows,
    // and one 500 ms ago has faded, though both are far after 40 ms.
    inputs.push("", 9_500, &msg(&[0x99, 36, 127]));
    inputs.push("", 9_500, &msg(&[0x89, 36, 0]));
    inputs.push("", 9_990, &msg(&[0x99, 37, 127]));
    inputs.push("", 9_990, &msg(&[0x89, 37, 0]));
    let frame = draw(&mut pads, ms(40));
    let lit = pixel(&frame, 15, 5);
    assert!(lit[0] > 200, "still lit: {lit:?}");
    assert_eq!(pixel(&frame, 5, 5), BLACK, "faded on the media clock");
}
