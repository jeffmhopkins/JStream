//! The audio scope's push and read, and painting and taking a waveform or spectrum
//! frame, of the program mix, of a bus (#362) or of another source (#459), do not
//! allocate once the frame pool has filled (R-RT-001 for the push, which runs on the
//! mixer thread; painting runs on the `jstream-sources` thread, but drawing 30 frames
//! a second stays allocation-free too).
//!
//! This test binary installs `assert_no_alloc`'s allocator as its global allocator, so
//! it is its own binary: no other test runs under it.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use jstream_media::{MediaTime, Timebase};
use jstream_protocol::{BusId, Settings, SourceId};
use jstream_sources::{AudioScope, AudioScopes, Context, Registry};
use serde_json::{Value, json};
use std::sync::Arc;

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn scope_pushes_reads_and_frames_do_not_allocate() {
    let scope = Arc::new(AudioScope::new());
    let scopes = Arc::new(AudioScopes::with_program(Arc::clone(&scope)));
    let bus = scopes.bus(&BusId::new("vod"));
    let mic = scopes.source(&SourceId::new("mic"));
    let block: Vec<f32> = (0..960).map(|n| (n as f32 * 0.05).sin()).collect();
    let mut window = vec![0.0f32; AudioScope::MAX_WINDOW];
    let registry = Registry::builtin();
    let ctx = Context {
        audio: Some(Arc::clone(&scopes)),
        ..Context::default()
    };
    let vod = json!({"type": "bus", "bus_id": "vod"});
    let from_mic = json!({"type": "source", "source_id": "mic"});
    let mut sources: Vec<_> = [
        ("waveform", Value::Null, Value::Null),
        ("spectrum", Value::Null, Value::Null),
        ("waveform", vod.clone(), Value::Null),
        ("spectrum", vod, Value::Null),
        ("waveform", Value::Null, from_mic.clone()),
        ("spectrum", Value::Null, from_mic),
    ]
    .into_iter()
    .map(|(kind, audio, audio_source)| {
        let mut settings: Settings = registry
            .descriptor(kind)
            .expect("a scope kind")
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect();
        settings.insert("audio", audio);
        settings.insert("audio_source", audio_source);
        registry.create(kind, &settings, &ctx).expect("a source")
    })
    .collect();
    let at = |tick: i64| MediaTime::new(tick * 34, Timebase::MILLIS);
    // Warm up: the first frames fill the frame pool.
    for tick in 0..4 {
        scope.push(48_000, &block);
        bus.push(48_000, &block);
        mic.push(48_000, &block);
        for source in &mut sources {
            if let Some(painter) = source.painter() {
                painter.paint(at(tick));
            }
            drop(source.frame(at(tick)).expect("a frame"));
        }
    }

    // The painters, taken outside the check: `painter()` hands out a new `Arc` handle.
    let painters: Vec<_> = sources
        .iter()
        .map(|s| s.painter().expect("a scope source paints"))
        .collect();
    reset_violation_count();
    for tick in 4..100 {
        assert_no_alloc(|| {
            scope.push(48_000, &block);
            bus.push(48_000, &block);
            mic.push(48_000, &block);
            scope.latest(&mut window);
            for (source, painter) in sources.iter_mut().zip(&painters) {
                painter.paint(at(tick));
                let frame = source.frame(at(tick)).expect("a frame");
                drop(frame);
            }
        });
    }
    assert_eq!(violation_count(), 0, "allocations while drawing");
}
