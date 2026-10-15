//! The audio scope's push and read, and a waveform or spectrum frame, do not allocate
//! (R-RT-001 for the push, which runs on the mixer thread; the frames run on the
//! `jstream-sources` thread, but drawing 30 a second stays allocation-free too).
//!
//! This test binary installs `assert_no_alloc`'s allocator as its global allocator, so
//! it is its own binary: no other test runs under it.

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use jstream_media::{MediaTime, Timebase};
use jstream_protocol::Settings;
use jstream_sources::{AudioScope, Context, Registry};
use std::sync::Arc;

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn scope_pushes_reads_and_frames_do_not_allocate() {
    let scope = Arc::new(AudioScope::new());
    let block: Vec<f32> = (0..960).map(|n| (n as f32 * 0.05).sin()).collect();
    let mut window = vec![0.0f32; AudioScope::MAX_WINDOW];
    let registry = Registry::builtin();
    let ctx = Context {
        audio: Some(Arc::clone(&scope)),
        ..Context::default()
    };
    let mut sources: Vec<_> = ["waveform", "spectrum"]
        .into_iter()
        .map(|kind| {
            let settings: Settings = registry
                .descriptor(kind)
                .expect("a scope kind")
                .params
                .iter()
                .map(|p| (p.id.clone(), p.default.clone()))
                .collect();
            registry.create(kind, &settings, &ctx).expect("a source")
        })
        .collect();
    let at = |tick: i64| MediaTime::new(tick * 34, Timebase::MILLIS);
    // Warm up: the first frames fill the frame pool.
    for tick in 0..4 {
        scope.push(48_000, &block);
        for source in &mut sources {
            drop(source.frame(at(tick)).expect("a frame"));
        }
    }

    reset_violation_count();
    for tick in 4..100 {
        assert_no_alloc(|| {
            scope.push(48_000, &block);
            scope.latest(&mut window);
            for source in &mut sources {
                // The frame handed out last stays held, as the program mix holds it.
                let frame = source.frame(at(tick)).expect("a frame");
                drop(frame);
            }
        });
    }
    assert_eq!(violation_count(), 0, "allocations while drawing");
}
