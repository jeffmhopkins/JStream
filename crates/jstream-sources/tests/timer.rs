//! The `timer` kind through the public API: a countdown and a stopwatch on the media
//! clock.

use jstream_media::{MediaTime, Timebase};
use jstream_protocol::Settings;
use jstream_sources::{Context, Registry, Source, needs_restart};
use serde_json::{Value, json};

fn settings(patch: &[(&str, Value)]) -> Settings {
    let registry = Registry::builtin();
    let kind = registry.descriptor("timer").expect("the timer kind");
    let mut settings: Settings = kind
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    for (id, value) in patch {
        settings.insert(*id, value.clone());
    }
    settings
}

fn ms(ms: i64) -> MediaTime {
    MediaTime::new(ms, Timebase::MILLIS)
}

/// The frame at `at` as its bytes, to compare what two times show.
fn shown(source: &mut Box<dyn Source>, at: i64) -> Vec<u8> {
    source.frame(ms(at)).expect("a frame").planes()[0]
        .data()
        .to_vec()
}

/// A text source drawing `text` with the same style, to compare against.
fn text(text: &str) -> Vec<u8> {
    let registry = Registry::builtin();
    let kind = registry.descriptor("text").expect("the text kind");
    let mut s: Settings = kind
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    s.insert("text", json!(text));
    let mut source = registry
        .create("text", &s, &Context::default())
        .expect("a text");
    source.frame(ms(0)).expect("a frame").planes()[0]
        .data()
        .to_vec()
}

#[test]
fn a_countdown_draws_each_second_as_text_and_stops_at_its_end() {
    let mut source = Registry::builtin()
        .create(
            "timer",
            &settings(&[("duration", json!(65)), ("end_text", json!("LIVE"))]),
            &Context::default(),
        )
        .expect("a timer");
    // Its clock starts at the first frame asked for, here 10 s into the source.
    assert_eq!(shown(&mut source, 10_000), text("01:05"));
    assert_eq!(source.next_change(ms(10_000)), Some(ms(11_000)));
    assert_eq!(shown(&mut source, 10_999), text("01:05"));
    assert_eq!(shown(&mut source, 11_000), text("01:04"));
    assert_eq!(source.next_change(ms(11_500)), Some(ms(12_000)));
    assert_eq!(shown(&mut source, 74_999), text("00:01"));
    assert_eq!(shown(&mut source, 75_000), text("LIVE"));
    assert_eq!(source.next_change(ms(75_000)), None);
}

#[test]
fn a_stopwatch_counts_up_and_a_new_duration_restarts_the_count() {
    let registry = Registry::builtin();
    let kind = registry.descriptor("timer").expect("the timer kind");
    let old = settings(&[("mode", json!("stopwatch"))]);
    let mut source = registry
        .create("timer", &old, &Context::default())
        .expect("a timer");
    assert_eq!(shown(&mut source, 0), text("00:00"));
    assert_eq!(shown(&mut source, 62_000), text("01:02"));
    // A style change keeps counting.
    let styled = settings(&[("mode", json!("stopwatch")), ("color", json!("#ffffffff"))]);
    assert!(!needs_restart(kind, &old, &styled));
    source.update(&styled).expect("a live update");
    assert_eq!(shown(&mut source, 63_000), text("01:03"));
    // A new mode starts again from the next frame.
    let countdown = settings(&[("duration", json!(10))]);
    source.update(&countdown).expect("a live update");
    assert_eq!(shown(&mut source, 70_000), text("00:10"));
    assert_eq!(shown(&mut source, 71_000), text("00:09"));
}
