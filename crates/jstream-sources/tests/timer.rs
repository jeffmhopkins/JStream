//! The `timer` kind through the public API (#1027): every frame one size whatever the
//! value, values drawn by its painter and never by `frame`, the frame sized for
//! `end_text` when it is created, and which changes keep its time zero.

use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::Settings;
use jstream_sources::{Context, Limits, Registry, Source, SourceError};
use serde_json::{Value, json};
use std::sync::Arc;

/// The timer kind's defaults with `patch` over them.
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

fn timer(patch: &[(&str, Value)]) -> Box<dyn Source> {
    Registry::builtin()
        .create("timer", &settings(patch), &Context::default())
        .expect("a timer")
}

fn ms(ms: i64) -> MediaTime {
    MediaTime::new(ms, Timebase::MILLIS)
}

/// The frame `source` shows at `at_ms`, once its painter has drawn it.
fn painted(source: &mut dyn Source, at_ms: i64) -> Arc<VideoFrame> {
    source.painter().expect("a painter").paint(ms(at_ms));
    source.frame(ms(at_ms)).expect("a frame")
}

fn size(frame: &VideoFrame) -> FrameSize {
    frame.info().size
}

/// What a `text` source draws for `text` with the timer's look: the value's own
/// frame, which is narrower than the timer's box.
fn text(text: &str) -> VideoFrame {
    let registry = Registry::builtin();
    let kind = registry.descriptor("text").expect("the text kind");
    let mut settings: Settings = kind
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    let timer = self::settings(&[]);
    for (id, value) in &timer.0 {
        if settings.0.contains_key(id) {
            settings.insert(id.as_str(), value.clone());
        }
    }
    settings.insert("text", json!(text));
    let mut source = registry
        .create("text", &settings, &Context::default())
        .expect("a text");
    (*source.frame(ms(0)).expect("a frame")).clone()
}

#[test]
fn a_countdown_keeps_its_size_when_it_turns_to_its_end_text() {
    let mut source = timer(&[("duration", json!(600)), ("end_text", json!("WE'RE LIVE"))]);
    let start = painted(source.as_mut(), 0);
    let left = painted(source.as_mut(), 599_500);
    let end = painted(source.as_mut(), 600_000);
    assert_eq!(size(&start), size(&left));
    assert_eq!(size(&start), size(&end));
    assert!(!Arc::ptr_eq(&left, &end), "the end text is a new frame");
    // The box holds the end text, which is wider than any value.
    assert!(size(&end).width() >= size(&text("WE'RE LIVE")).width());
}

#[test]
fn a_stopwatch_keeps_its_size_past_an_hour() {
    let mut source = timer(&[("mode", json!("stopwatch"))]);
    let before = painted(source.as_mut(), 3_599_000);
    let after = painted(source.as_mut(), 3_600_000);
    assert_eq!(size(&before), size(&after));
    assert!(size(&before).width() >= size(&text("8:88:88")).width());
}

#[test]
fn a_clock_has_one_size_and_changes_with_the_wall_clock() {
    let mut source = timer(&[("mode", json!("clock")), ("clock_format", json!("12h"))]);
    let first = source.frame(ms(0)).expect("a frame");
    let later = painted(source.as_mut(), 0);
    assert_eq!(size(&first), size(&later));
    // Within the next wall-clock second; `ms(0)` itself when a second boundary passed
    // since the paint, which asks for another at once.
    let next = source.next_change(ms(0)).expect("a clock changes");
    assert!(next <= ms(1_000), "{next:?}");
}

#[test]
fn frame_hands_out_what_the_painter_drew() {
    let mut source = timer(&[("mode", json!("stopwatch"))]);
    let zero = source.frame(ms(0)).expect("a frame");
    // Asking for a later frame doesn't draw it: that is the painter's job, off the
    // engine's lock, so `frame` hands out the one drawn last and asks for a paint now.
    let unpainted = source.frame(ms(5_000)).expect("a frame");
    assert!(Arc::ptr_eq(&zero, &unpainted));
    assert_eq!(source.next_change(ms(5_000)), Some(ms(5_000)));
    let five = painted(source.as_mut(), 5_000);
    assert!(!Arc::ptr_eq(&zero, &five));
    assert_eq!(source.next_change(ms(5_000)), Some(ms(6_000)));
    // A paint within the same second draws nothing new.
    let same = painted(source.as_mut(), 5_999);
    assert!(Arc::ptr_eq(&five, &same));
}

#[test]
fn a_countdown_stops_changing_at_its_end() {
    let mut source = timer(&[("duration", json!(2))]);
    assert_eq!(source.next_change(ms(0)), Some(ms(1_000)));
    painted(source.as_mut(), 2_000);
    assert_eq!(source.next_change(ms(2_000)), None);
}

#[test]
fn an_end_text_too_large_for_the_limits_fails_at_create_and_update() {
    let limits = Limits {
        max_side: 2_000,
        ..Limits::default()
    };
    let ctx = Context {
        limits,
        ..Context::default()
    };
    let wide = "LIVE ".repeat(50);
    let refused =
        Registry::builtin().create("timer", &settings(&[("end_text", json!(wide))]), &ctx);
    assert!(
        matches!(refused, Err(SourceError::Setting { ref param, .. }) if param == "end_text"),
        "{:?}",
        refused.err()
    );
    let mut source = Registry::builtin()
        .create("timer", &settings(&[]), &ctx)
        .expect("a timer");
    assert!(matches!(
        source.update(&settings(&[("end_text", json!(wide))])),
        Err(SourceError::Setting { .. })
    ));
}

/// A box whose text fits `max_side` but whose padding doesn't is blamed on a timer
/// setting, not on the text renderer's own `text` (review N2 on #1039).
#[test]
fn a_frame_too_large_names_a_timer_setting() {
    let ctx = Context {
        limits: Limits {
            max_side: 1_000,
            ..Limits::default()
        },
        ..Context::default()
    };
    let refused = Registry::builtin().create(
        "timer",
        &settings(&[("end_text", json!("LIVE NOW")), ("padding", json!(400))]),
        &ctx,
    );
    assert!(
        matches!(refused, Err(SourceError::Setting { ref param, .. }) if param == "end_text"),
        "{:?}",
        refused.err()
    );
}

#[test]
fn a_density_draws_more_pixels_for_the_same_box() {
    let mut source = timer(&[]);
    let one = source.frame(ms(0)).expect("a frame");
    let extent = source.set_density(2.0).expect("drawn at a density");
    assert_eq!(extent, size(&one));
    let two = source.frame(ms(0)).expect("a frame");
    assert_eq!(
        (size(&two).width(), size(&two).height()),
        (extent.width() * 2, extent.height() * 2)
    );
}

#[test]
fn a_live_update_redraws_the_value_shown() {
    let mut source = timer(&[("mode", json!("stopwatch"))]);
    let three = painted(source.as_mut(), 3_000);
    source
        .update(&settings(&[
            ("mode", json!("stopwatch")),
            ("color", json!("#ff0000ff")),
        ]))
        .expect("color is live");
    let red = source.frame(ms(3_000)).expect("a frame");
    assert!(!Arc::ptr_eq(&three, &red));
    assert_eq!(size(&three), size(&red));
    assert_eq!(
        source.next_change(ms(3_000)),
        Some(ms(4_000)),
        "still 00:03"
    );
    assert!(matches!(
        source.update(&settings(&[("mode", json!("countdown"))])),
        Err(SourceError::NotLive { .. })
    ));
}

#[test]
fn only_mode_duration_and_restart_restart_the_count() {
    let registry = Registry::builtin();
    let old = settings(&[]);
    let keeps = |patch: &[(&str, Value)]| registry.keeps_time("timer", &old, &settings(patch));
    assert!(keeps(&[("color", json!("#ff0000ff"))]));
    assert!(keeps(&[("face", json!("bold"))]));
    assert!(keeps(&[("end_text", json!("LIVE"))]));
    assert!(!keeps(&[("mode", json!("stopwatch"))]));
    assert!(!keeps(&[("duration", json!(60))]));
    assert!(
        !keeps(&[("restart", json!(1))]),
        "#1040: the restart counter"
    );
    assert!(
        !registry.keeps_time("text", &old, &old),
        "other kinds start again"
    );
}

#[test]
fn an_unknown_time_zone_is_refused() {
    let refused = Registry::builtin().create(
        "timer",
        &settings(&[
            ("mode", json!("clock")),
            ("time_zone", json!("Mars/Olympus")),
        ]),
        &Context::default(),
    );
    assert!(matches!(
        refused,
        Err(SourceError::Setting { ref param, .. }) if param == "time_zone"
    ));
}
