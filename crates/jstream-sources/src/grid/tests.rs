use super::*;
use crate::source::at_millis;
use crate::{Registry, Source};
use serde_json::{Value, json};

/// The defaults of `grid` with `overrides` on them.
fn settings(overrides: Value) -> Settings {
    let mut settings: Settings = Registry::builtin()
        .descriptor("grid")
        .expect("the grid kind")
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

fn source(data: &LiveData, overrides: Value) -> Box<dyn Source> {
    let ctx = Context {
        data: Some(data.clone()),
        ..Context::default()
    };
    Registry::builtin()
        .create("grid", &settings(overrides), &ctx)
        .expect("a grid source")
}

fn draw(source: &mut Box<dyn Source>, ms: u64) -> Arc<VideoFrame> {
    let at = at_millis(ms);
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

fn put(data: &LiveData, key: &str, value: Value) {
    data.put("Bitwig", key, value.to_string().as_bytes(), 0)
        .expect("taken");
}

/// A 2 x 2 grid, 100 px cells with no gap or rounding, so cell (c, r)'s centre is at
/// (50 + 100c, 50 + 100r).
fn two_by_two(extra: Value) -> Value {
    let mut base = json!({
        "width": 200, "height": 200, "columns": 2, "rows": 2, "gap": 0, "radius": 0,
        "state_field": "queued,state",
        "looks": {
            "absent": {"color": "#000000"},
            "playing": {"color": "cell"},
            "stopped": {"color": "cell", "brightness": 0.5},
            "recording": {"color": "#ff0000"},
            "play": {"color": "#00ff00", "blink_hz": 2},
        },
    });
    if let (Value::Object(b), Value::Object(e)) = (&mut base, extra) {
        b.extend(e);
    }
    base
}

#[test]
fn each_cell_takes_the_look_of_its_state_and_its_own_colour() {
    let data = LiveData::new();
    put(
        &data,
        "track/0",
        json!({"slots": [
            {"state": "playing", "color": "#4080ff"},
            {"state": "stopped", "color": "#4080ff"},
        ]}),
    );
    put(
        &data,
        "track/1",
        json!({"slots": [{"state": "recording"}, {"state": "mystery", "color": "#ffffff"}]}),
    );
    let mut grid = source(&data, two_by_two(json!({"dim": 0.1})));
    let frame = draw(&mut grid, 0);
    assert_eq!(
        pixel(&frame, 50, 50),
        [0x40, 0x80, 0xff, 0xff],
        "playing: its own"
    );
    assert_eq!(
        pixel(&frame, 50, 150),
        [0x20, 0x40, 0x80, 0xff],
        "stopped at half"
    );
    assert_eq!(
        pixel(&frame, 150, 50),
        [0xff, 0, 0, 0xff],
        "recording: fixed red"
    );
    assert_eq!(
        pixel(&frame, 150, 150),
        [26, 26, 26, 0xff],
        "no look: dim own"
    );
}

#[test]
fn a_cell_the_data_lacks_is_absent_and_the_first_state_with_a_look_wins() {
    let data = LiveData::new();
    put(
        &data,
        "track/0",
        json!({"slots": [{"state": "stopped", "queued": "play", "color": "#ffffff"}]}),
    );
    let mut grid = source(&data, two_by_two(json!({})));
    let frame = draw(&mut grid, 0);
    assert_eq!(
        pixel(&frame, 50, 50),
        [0, 0xff, 0, 0xff],
        "queued blinks on"
    );
    assert_eq!(pixel(&frame, 50, 150), [0, 0, 0, 0xff], "no entry: absent");
    assert_eq!(pixel(&frame, 150, 50), [0, 0, 0, 0xff], "no column: absent");
    // Two blinks a second: off in each period's second half.
    let frame = draw(&mut grid, 300);
    assert_eq!(pixel(&frame, 50, 50), [0, 0x40, 0, 0xff], "and off");
    // Without the queue, the state decides.
    put(
        &data,
        "track/0",
        json!({"slots": [{"state": "stopped", "queued": null, "color": "#ffffff"}]}),
    );
    let frame = draw(&mut grid, 600);
    assert_eq!(pixel(&frame, 50, 50), [0x80, 0x80, 0x80, 0xff]);
}

#[test]
fn offsets_pick_which_columns_and_rows_are_drawn() {
    let data = LiveData::new();
    put(
        &data,
        "track/5",
        json!({"slots": [{}, {}, {}, {"state": "recording"}]}),
    );
    let mut grid = source(
        &data,
        two_by_two(json!({"column_offset": 5, "row_offset": 3})),
    );
    let frame = draw(&mut grid, 0);
    assert_eq!(pixel(&frame, 50, 50), [0xff, 0, 0, 0xff]);
    assert_eq!(pixel(&frame, 50, 150), [0, 0, 0, 0xff]);
}

#[test]
fn a_change_to_the_data_is_drawn_on_the_next_tick() {
    let data = LiveData::new();
    // No look blinks, so only new data draws again.
    let looks = json!({"absent": {"color": "#000000"}, "recording": {"color": "#ff0000"}});
    let mut grid = source(&data, two_by_two(json!({ "looks": looks })));
    let first = draw(&mut grid, 0);
    assert_eq!(pixel(&first, 50, 50), [0, 0, 0, 0xff]);
    let again = draw(&mut grid, 40);
    assert!(Arc::ptr_eq(&first, &again), "nothing new: the same frame");
    put(&data, "track/0", json!({"slots": [{"state": "recording"}]}));
    let next = draw(&mut grid, 80);
    assert_eq!(pixel(&next, 50, 50), [0xff, 0, 0, 0xff]);
    assert!(grid.next_change(at_millis(80)).is_some());
}

#[test]
fn a_sender_by_name_draws_only_its_own_data() {
    let data = LiveData::new();
    data.put(
        "Other",
        "track/0",
        br#"{"slots":[{"state":"recording"}]}"#,
        0,
    )
    .expect("taken");
    let mut named = source(&data, two_by_two(json!({"input": "Bitwig"})));
    assert_eq!(pixel(&draw(&mut named, 0), 50, 50), [0, 0, 0, 0xff]);
    let mut any = source(&data, two_by_two(json!({})));
    assert_eq!(pixel(&draw(&mut any, 0), 50, 50), [0xff, 0, 0, 0xff]);
}

#[test]
fn bars_rise_with_their_level_and_a_list_entry_draws_side_by_side() {
    let data = LiveData::new();
    put(&data, "meters", json!({"tracks": [0.5, [1.0, 0.0]]}));
    let mut grid = source(
        &data,
        json!({
            "mode": "bars", "width": 200, "height": 100, "columns": 2, "gap": 0,
            "radius": 0, "pad": "#000000ff", "color_low": "#00ff00ff",
            "color_high": "#00ff00ff",
        }),
    );
    let frame = draw(&mut grid, 0);
    assert_eq!(
        pixel(&frame, 50, 90),
        [0, 0xff, 0, 0xff],
        "half: lit at the foot"
    );
    assert_eq!(pixel(&frame, 50, 10), [0, 0, 0, 0xff], "and not at the top");
    // The second column's two bars split it, with a 1 px gap between.
    assert_eq!(pixel(&frame, 120, 5), [0, 0xff, 0, 0xff], "left full");
    assert_eq!(pixel(&frame, 180, 90), [0, 0, 0, 0xff], "right empty");
}

#[test]
fn segmented_bars_light_whole_steps() {
    let data = LiveData::new();
    put(&data, "meters", json!({"tracks": [0.55]}));
    let mut grid = source(
        &data,
        json!({
            "mode": "bars", "width": 20, "height": 100, "columns": 1, "gap": 0,
            "radius": 0, "segments": 4, "pad": "#000000ff",
            "color_low": "#ffffffff", "color_high": "#ffffffff",
        }),
    );
    let frame = draw(&mut grid, 0);
    // Four steps of 25 px; 0.55 lights the lower two (each lit from its midpoint).
    assert_eq!(pixel(&frame, 10, 90), [0xff, 0xff, 0xff, 0xff]);
    assert_eq!(pixel(&frame, 10, 60), [0xff, 0xff, 0xff, 0xff]);
    assert_eq!(pixel(&frame, 10, 40), [0, 0, 0, 0xff]);
}

#[test]
fn without_data_every_cell_is_absent_and_nothing_is_due() {
    let ctx = Context::default();
    let mut grid = Registry::builtin()
        .create("grid", &settings(two_by_two(json!({}))), &ctx)
        .expect("a grid source");
    assert_eq!(pixel(&draw(&mut grid, 0), 50, 50), [0, 0, 0, 0xff]);
    assert_eq!(grid.next_change(at_millis(0)), None);
}

#[test]
fn a_bad_look_or_level_range_is_refused() {
    let data = LiveData::new();
    let ctx = Context {
        data: Some(data),
        ..Context::default()
    };
    let refused = |overrides: Value| {
        Registry::builtin()
            .create("grid", &settings(overrides), &ctx)
            .is_err()
    };
    assert!(refused(json!({"looks": {"x": {"color": "red"}}})));
    assert!(refused(json!({"low": 1.0, "high": 1.0})));
    assert!(!refused(json!({"looks": {}})));
}
