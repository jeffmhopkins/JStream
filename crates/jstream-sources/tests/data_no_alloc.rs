//! A live-data watch's cell is read without a lock or an allocation (ADR-0171
//! decision 2, R-RT-003): the render thread only loads it.
//!
//! This test binary installs `assert_no_alloc`'s allocator as its global allocator, so
//! it is its own binary: no other test runs under it.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use jstream_sources::{DataReading, DataWatch, LiveData, text_hash};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn reading_a_watched_field_does_not_allocate() {
    let data = LiveData::new();
    let watch = |field: &str, color: bool| {
        data.watch(&DataWatch {
            input: Some("Bitwig".into()),
            key: "track/0".into(),
            field: field.into(),
            color,
        })
    };
    let volume = watch("volume", false);
    let state = watch("slots.0.state", false);
    let color = watch("color", true);
    data.put(
        "Bitwig",
        "track/0",
        br##"{"volume":0.5,"color":"#ff8000","slots":[{"state":"playing"}]}"##,
        0,
    )
    .expect("taken");
    reset_violation_count();
    let read = assert_no_alloc(|| [volume.read(), state.read(), color.read()]);
    assert_eq!(violation_count(), 0);
    assert_eq!(
        read,
        [
            DataReading::Number(0.5),
            DataReading::Text(text_hash("playing")),
            DataReading::Color([255, 128, 0, 255]),
        ]
    );
}
