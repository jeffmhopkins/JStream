use super::*;
use crate::{Context, Registry, Source};
use serde_json::{Value, json};
use std::thread;

const RATE: u32 = 48_000;

/// Interleaved stereo with both channels at `mono[n]`.
fn stereo(mono: impl IntoIterator<Item = f32>) -> Vec<f32> {
    mono.into_iter().flat_map(|s| [s, s]).collect()
}

fn ms(ms: u64) -> MediaTime {
    at_millis(ms)
}

#[test]
fn the_ring_reads_the_latest_window_as_mono() {
    let scope = AudioScope::new();
    let mut out = [9.0; 4];
    assert_eq!(scope.latest(&mut out), (0, 0), "nothing pushed yet");
    assert_eq!(out, [0.0; 4]);

    // Left and right average to mono; a trailing half frame is ignored.
    scope.push(RATE, &[1.0, 0.0, 0.5, 0.5, -1.0]);
    assert_eq!(scope.latest(&mut out), (2, RATE));
    assert_eq!(
        out,
        [0.0, 0.0, 0.5, 0.5],
        "samples before the first push are 0"
    );

    // Past the ring's capacity, the window is still the latest samples in order.
    let all: Vec<f32> = (0..CAPACITY as u32 * 3).map(|n| n as f32).collect();
    for block in all.chunks(480) {
        scope.push(RATE, &stereo(block.iter().copied()));
    }
    let mut window = vec![0.0; WINDOW];
    let (written, _) = scope.latest(&mut window);
    assert_eq!(written, 2 + all.len() as u64);
    assert_eq!(window, all[all.len() - WINDOW..]);
}

#[test]
fn a_reader_never_sees_a_window_a_push_overwrote() {
    // The writer pushes a ramp, so any copied window must be consecutive numbers (or
    // silence, when pushes keep overtaking the copy).
    let scope = Arc::new(AudioScope::new());
    let writer = {
        let scope = Arc::clone(&scope);
        thread::spawn(move || {
            let mut n = 0u32;
            // f32 holds whole numbers exactly up to 2^24.
            while n < (1 << 22) {
                scope.push(RATE, &stereo((n..n + 256).map(|s| s as f32)));
                n += 256;
            }
        })
    };
    let mut window = vec![0.0; WINDOW];
    let mut checked = 0;
    while !writer.is_finished() {
        let (written, rate) = scope.latest(&mut window);
        if rate == 0 || written < WINDOW as u64 {
            continue;
        }
        let first = window[0];
        for (i, &s) in window.iter().enumerate() {
            assert_eq!(s, first + i as f32, "a torn window at {i}: {window:?}");
        }
        checked += 1;
    }
    writer.join().expect("the writer");
    assert!(checked > 0, "no window was checked");
}

#[test]
fn falls_ease_and_rises_are_immediate() {
    assert_eq!(ease(0.2, 0.8, 0.9, 1), 0.8);
    assert_eq!(ease(1.0, 0.0, 0.0, 1), 0.0);
    assert!((ease(1.0, 0.0, 0.5, 1) - 0.5).abs() < 1e-6);
    assert!((ease(1.0, 0.0, 0.5, 3) - 0.125).abs() < 1e-6);
}

#[test]
fn frame_ticks_are_a_thirtieth_of_a_second() {
    assert_eq!(tick(ms(0)), 0);
    assert_eq!(tick(ms(33)), 0);
    assert_eq!(tick(ms(34)), 1);
    assert_eq!(next_tick(ms(0)), ms(34));
    assert_eq!(next_tick(ms(990)), ms(1000));
    assert_eq!(next_tick(ms(1000)), ms(1034));
}

/// A scope source of `kind` reading `scope`, with `overrides` on its defaults.
fn source(kind: &str, scope: Option<Arc<AudioScope>>, overrides: Value) -> Box<dyn Source> {
    let registry = Registry::builtin();
    let mut settings: Settings = registry
        .descriptor(kind)
        .expect("a scope kind")
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    if let Value::Object(map) = overrides {
        for (k, v) in map {
            settings.insert(k, v);
        }
    }
    let ctx = Context {
        audio: scope,
        ..Context::default()
    };
    registry
        .create(kind, &settings, &ctx)
        .expect("a scope source")
}

fn pixel(frame: &VideoFrame, x: u32, y: u32) -> [u8; 4] {
    let at = x as usize * 4;
    let row = frame.row(0, y).expect("a row");
    [row[at], row[at + 1], row[at + 2], row[at + 3]]
}

const RED: [u8; 4] = [0xff, 0, 0, 0xff];
const BLUE: [u8; 4] = [0, 0, 0xff, 0xff];

#[test]
fn the_waveform_draws_each_column_from_its_lowest_to_its_highest_sample() {
    let scope = Arc::new(AudioScope::new());
    // The first half of the window at +0.5, the second at -0.5.
    let half = WINDOW / 2;
    scope.push(
        RATE,
        &stereo((0..WINDOW).map(|n| if n < half { 0.5 } else { -0.5 })),
    );
    let mut waveform = source(
        "waveform",
        Some(Arc::clone(&scope)),
        json!({"width": 8, "height": 9, "color": "#ff0000", "background": "#0000ff"}),
    );
    let frame = waveform.frame(ms(0)).expect("a frame");
    // Level 1 is row 0, 0 is row 4 and -1 is row 8.
    for x in 0..8 {
        let lit = if x < 4 { 2 } else { 6 };
        for y in 0..9 {
            let want = if y == lit { RED } else { BLUE };
            assert_eq!(pixel(&frame, x, y), want, "pixel {x},{y}");
        }
    }
    // Within the same frame tick, the same frame.
    assert!(Arc::ptr_eq(
        &frame,
        &waveform.frame(ms(20)).expect("a frame")
    ));
    assert_eq!(waveform.next_change(ms(20)), Some(ms(34)));
}

#[test]
fn the_db_scale_lifts_quiet_audio() {
    let scope = Arc::new(AudioScope::new());
    // -20 dBFS is a third of the way up from -60.
    scope.push(RATE, &stereo((0..WINDOW).map(|_| 0.1)));
    let draw = |scale: &str| {
        let mut waveform = source(
            "waveform",
            Some(Arc::clone(&scope)),
            json!({"width": 1, "height": 13, "color": "#ff0000", "background": "#0000ff", "scale": scale}),
        );
        let frame = waveform.frame(ms(0)).expect("a frame");
        (0..13).position(|y| pixel(&frame, 0, y) == RED)
    };
    // Rows run 0 (level 1) to 12 (level -1); 0.1 rounds to row 5 and 2/3 to row 2.
    assert_eq!(draw("linear"), Some(5));
    assert_eq!(draw("db"), Some(2));
}

#[test]
fn the_spectrum_lights_the_bar_holding_a_tone() {
    let scope = Arc::new(AudioScope::new());
    // A full-scale sine at bin 75: 1758 Hz, mid-bar, so the Hann window's main lobe
    // (two bins either side) stays inside its bar.
    let hz = 75.0 * RATE as f32 / WINDOW as f32;
    scope.push(
        RATE,
        &stereo((0..WINDOW).map(|n| (std::f32::consts::TAU * hz * n as f32 / RATE as f32).sin())),
    );
    // Linear from 0 to 20 kHz in 40 bars of 500 Hz: the tone is in bar 3.
    let mut spectrum = source(
        "spectrum",
        Some(Arc::clone(&scope)),
        json!({"width": 40, "height": 100, "bars": 40, "scale": "linear",
               "color": "#ff0000", "background": "#0000ff", "smoothing": 0.0}),
    );
    let frame = spectrum.frame(ms(0)).expect("a frame");
    let bar = |x: u32| (0..100).filter(|&y| pixel(&frame, x, y) == RED).count();
    assert!(
        bar(3) >= 98,
        "the tone's bar is near full height: {}",
        bar(3)
    );
    for x in (0..40).filter(|&x| x != 3) {
        // Ten bins out, the Hann window's sidelobes are about 70 dB down.
        assert!(bar(x) < bar(3) / 2, "bar {x} is {} high", bar(x));
    }
}

#[test]
fn every_log_bar_shows_even_below_a_bins_width() {
    let scope = Arc::new(AudioScope::new());
    // White-ish noise: every bin has energy.
    let mut seed = 1u32;
    scope.push(
        RATE,
        &stereo((0..WINDOW).map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 23) as f32 - 1.0
        })),
    );
    let mut spectrum = source(
        "spectrum",
        Some(scope),
        json!({"width": 128, "height": 50, "bars": 128, "color": "#ff0000",
               "background": "#0000ff"}),
    );
    let frame = spectrum.frame(ms(0)).expect("a frame");
    for x in 0..128 {
        assert_eq!(pixel(&frame, x, 49), RED, "bar {x} is empty");
    }
}

#[test]
fn bars_fall_back_at_the_smoothing_rate_and_a_still_ring_reads_as_silence() {
    let scope = Arc::new(AudioScope::new());
    let loud = stereo((0..WINDOW).map(|n| if n % 2 == 0 { 1.0 } else { -1.0 }));
    scope.push(RATE, &loud);
    let mut waveform = source(
        "waveform",
        Some(Arc::clone(&scope)),
        json!({"width": 1, "height": 101, "color": "#ff0000", "background": "#0000ff",
               "smoothing": 0.5}),
    );
    let lit = |frame: &VideoFrame| (0..101).filter(|&y| pixel(frame, 0, y) == RED).count();
    assert_eq!(lit(&waveform.frame(ms(0)).expect("a frame")), 101);
    // The ring hasn't moved for 250 ms: silence, eased by half a tick at a time.
    let held = waveform.frame(ms(250)).expect("a frame");
    // 7 ticks later the peak is at 0.5^7 of full scale: within a row of the centre.
    assert!(lit(&held) <= 3, "{} rows lit", lit(&held));
    // A frame handed out stays as it was when the next is drawn.
    let before: Vec<u8> = held.row(0, 0).expect("a row").to_vec();
    scope.push(RATE, &loud);
    let after = waveform.frame(ms(300)).expect("a frame");
    assert_eq!(lit(&after), 101);
    assert!(!Arc::ptr_eq(&held, &after));
    assert_eq!(held.row(0, 0).expect("a row"), before.as_slice());
}

#[test]
fn without_a_scope_the_sources_draw_silence_once() {
    // Silence is the waveform's centre line (row 2 of rows 0 to 3, rounded) and no bars.
    for (kind, line) in [("waveform", Some(2)), ("spectrum", None)] {
        let mut s = source(
            kind,
            None,
            json!({"width": 4, "height": 4, "color": "#ff0000", "background": "#0000ff"}),
        );
        let frame = s.frame(ms(0)).expect("a frame");
        for (x, y) in (0..4).flat_map(|x| (0..4).map(move |y| (x, y))) {
            let want = if Some(y) == line { RED } else { BLUE };
            assert_eq!(pixel(&frame, x, y), want, "{kind} {x},{y}");
        }
        assert_eq!(s.next_change(ms(0)), None, "{kind}");
    }
}

#[test]
fn colours_bars_smoothing_and_scale_are_live_and_size_is_not() {
    for kind in ["waveform", "spectrum"] {
        let mut s = source(kind, None, json!({}));
        let registry = Registry::builtin();
        let mut settings: Settings = registry
            .descriptor(kind)
            .expect("a kind")
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect();
        settings.insert("color", json!("#00ff00"));
        settings.insert("smoothing", json!(0.2));
        if kind == "spectrum" {
            settings.insert("bars", json!(8));
            settings.insert("scale", json!("linear"));
        } else {
            settings.insert("scale", json!("db"));
        }
        s.update(&settings).expect("a live update");
        settings.insert("smoothing", json!(1.0));
        assert!(
            matches!(s.update(&settings), Err(SourceError::Setting { .. })),
            "{kind}"
        );
        settings.insert("smoothing", json!(0.2));
        settings.insert("width", json!(100));
        assert!(
            matches!(s.update(&settings), Err(SourceError::NotLive { .. })),
            "{kind}"
        );
    }
}

#[test]
fn a_frame_size_must_fit_four_frames_in_the_decode_limit() {
    let registry = Registry::builtin();
    let settings: Settings = registry
        .descriptor("spectrum")
        .expect("a kind")
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    let ctx = Context {
        limits: Limits {
            // Three 640x240 frames, not four.
            max_decoded_bytes: 640 * 240 * 4 * 3,
            ..Limits::default()
        },
        ..Context::default()
    };
    assert!(matches!(
        registry.create("spectrum", &settings, &ctx),
        Err(SourceError::Setting { param, .. }) if param == "width"
    ));
}
