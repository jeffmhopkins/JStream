//! The profiler's windows, from meters driven by hand (ADR-0103).

use std::sync::Arc;
use std::time::{Duration, Instant};

use jstream_platform::ThreadTime;
use jstream_platform::fake::FakeThreads;
use jstream_protocol::{Drawer, ItemId, ProfileUnit, SceneId, SourceId};

use super::meter::{BUCKETS, bucket_top, histogram_bucket};
use super::{Phase, Profiler, SourceMeters};
use crate::chain::ChainOwner;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Every frame length falls in a bucket whose top is above it, and the buckets rise.
#[test]
fn histogram_buckets_hold_their_frames() {
    for nanos in [
        0, 1, 249_999, 250_000, 15_999_999, 16_000_000, 63_999_999, 64_000_000,
    ] {
        let bucket = histogram_bucket(nanos);
        assert!(bucket < BUCKETS);
        assert!(bucket_top(bucket) > nanos, "{nanos} in {bucket}");
    }
    assert_eq!(histogram_bucket(10_000_000_000), BUCKETS - 1);
    for bucket in 1..BUCKETS {
        assert!(bucket_top(bucket) >= bucket_top(bucket - 1), "{bucket}");
    }
}

/// A drawer's units come out costliest first with their share of its time, its frames
/// against the budget, and its phases per frame.
#[test]
fn a_window_ranks_the_units_of_a_drawer() {
    let profiler = Profiler::new(ms(33), Arc::new(SourceMeters::default()));
    let mut meter = profiler.program_meter();
    let item = ChainOwner::Item(SceneId::new("main"), ItemId::new("glow"));
    let start = Instant::now();
    // Ten frames: the item's effects take 20 ms, an upload 5 ms; every other frame is
    // over the 33 ms budget.
    for frame in 0..10u64 {
        let begun = Instant::now();
        meter.chain(
            &item,
            begun.checked_sub(ms(20)).expect("an earlier instant"),
        );
        if frame % 2 == 0 {
            meter.upload(
                &SourceId::new("cam"),
                begun.checked_sub(ms(5)).expect("earlier"),
            );
        }
        meter.phase(Phase::Chains, ms(20));
        let took = if frame % 2 == 0 { ms(40) } else { ms(25) };
        meter.skip(u64::from(frame == 3));
        meter.end_frame(took, start + ms(1000 * frame), drop);
    }
    let info = profiler.profile(Some(5), None, None);
    let program = info
        .drawers
        .iter()
        .find(|d| d.drawer == Drawer::Program)
        .expect("the program drew");
    assert!(program.frames >= 1);
    assert_eq!(program.over_budget, 5.min(program.frames));
    assert!(program.phases.chains_ms.get() >= 19.9);
    let first = info.units.first().expect("a unit");
    assert_eq!(
        first.unit,
        ProfileUnit::ItemEffects {
            scene_id: SceneId::new("main"),
            item_id: ItemId::new("glow"),
        }
    );
    assert!(first.ms.get() >= 20.0, "{first:?}");
    assert!(
        first.share.get() > 0.5 && first.share.get() <= 1.0,
        "{first:?}"
    );
    let upload = info
        .units
        .iter()
        .find(
            |u| matches!(&u.unit, ProfileUnit::Upload { source_id } if source_id.as_str() == "cam"),
        )
        .expect("the upload");
    assert!(upload.frames < first.frames, "it ran in every other frame");
    assert!((info.frame_budget_ms.get() - 33.0).abs() < 0.01);
    assert!(!info.gpu_timing);
}

/// A unit in a later window counts only what it did since the earlier one, and a new
/// run of the drawer counts from zero.
#[test]
fn windows_subtract_the_earlier_reading() {
    let profiler = Profiler::new(ms(33), Arc::new(SourceMeters::default()));
    let mut meter = profiler.program_meter();
    let scene = SceneId::new("a");
    let start = Instant::now();
    meter.pass(&scene, ms(4));
    meter.end_frame(ms(5), start, drop);
    meter.pass(&scene, ms(4));
    meter.end_frame(ms(5), start + ms(1000), drop);
    // The earlier reading: two frames.
    profiler.sample(None);
    std::thread::sleep(ms(10));
    meter.pass(&scene, ms(8));
    meter.end_frame(ms(9), start + ms(2000), drop);
    let info = profiler.profile(Some(1), None, None);
    let program = &info.drawers[0];
    assert_eq!(
        program.frames, 1,
        "only the frame since the reading: {info:?}"
    );
    assert!((info.units[0].ms.get() - 8.0).abs() < 0.01, "{info:?}");
    // A new run publishes totals from zero: nothing is subtracted from them.
    let mut again = profiler.program_meter();
    again.pass(&scene, ms(2));
    again.end_frame(ms(3), start, drop);
    again.pass(&scene, ms(2));
    again.end_frame(ms(3), start + ms(1000), drop);
    let info = profiler.profile(Some(1), None, None);
    assert_eq!(info.drawers[0].frames, 2, "{info:?}");
}

/// Threads of one name are added together; JStream's own are `name`, others
/// `remote_name`, and the CPU is a percentage of one core over the window.
#[test]
fn threads_are_summed_by_name() {
    let profiler = Profiler::new(ms(33), Arc::new(SourceMeters::default()));
    let threads = FakeThreads::new();
    profiler.set_threads(Arc::new(threads.clone()));
    let thread = |id, name: &str, cpu| ThreadTime {
        id,
        name: name.into(),
        cpu: ms(cpu),
    };
    threads.set(Some(vec![
        thread(1, "jstream-render", 0),
        thread(2, "jstream-media-file", 0),
        thread(3, "jstream-media-file", 0),
        thread(4, "nvidia-worker", 0),
    ]));
    profiler.sample(None);
    std::thread::sleep(ms(100));
    threads.set(Some(vec![
        thread(1, "jstream-render", 50),
        thread(2, "jstream-media-file", 20),
        thread(3, "jstream-media-file", 20),
        thread(4, "nvidia-worker", 5),
        thread(5, "", 1),
    ]));
    let info = profiler.profile(Some(1), Some(3), None);
    assert!(info.thread_cpu);
    assert_eq!(info.threads.len(), 3, "cut at top: {info:?}");
    let render = &info.threads[0];
    assert_eq!(render.name.as_deref(), Some("jstream-render"));
    assert!(render.cpu_pct.get() > 10.0, "{render:?}");
    let media = &info.threads[1];
    assert_eq!(media.name.as_deref(), Some("jstream-media-file"));
    assert_eq!(media.count, 2);
    let driver = &info.threads[2];
    assert_eq!(driver.name, None);
    assert_eq!(driver.remote_name.as_deref(), Some("nvidia-worker"));
}

/// The sources thread's paints and builds and the planner's looks come out per source,
/// costliest first, and a deleted source is forgotten.
#[test]
fn sources_and_plans_are_counted() {
    let meters = Arc::new(SourceMeters::default());
    let profiler = Profiler::new(ms(33), Arc::clone(&meters));
    let wave = SourceId::new("wave");
    let text = SourceId::new("text");
    meters.painted(&wave, ms(3));
    meters.painted(&wave, ms(5));
    meters.looked(&wave, ms(1));
    meters.looked(&text, ms(1));
    meters.built(&text, ms(12));
    meters.planned(ms(2));
    meters.planned(ms(6));
    let info = profiler.profile(None, None, None);
    assert_eq!(info.plan.plans, 2);
    assert!((info.plan.ms.get() - 4.0).abs() < 0.01);
    assert!((info.plan.recent_max_ms.get() - 6.0).abs() < 0.01);
    let first = &info.sources[0];
    assert_eq!(first.source_id, text, "its build cost the most: {info:?}");
    assert_eq!(first.build_ms.map(|b| b.get().round()), Some(12.0));
    let wave_info = &info.sources[1];
    assert_eq!(wave_info.paints, 2);
    assert!((wave_info.paint_ms.get() - 4.0).abs() < 0.01);
    meters.retain(|id| *id != text);
    let info = profiler.profile(None, None, None);
    assert!(info.sources.iter().all(|s| s.source_id != text));
}

/// With no reading yet the window covers the time since the profiler started, and a
/// window is capped at 60 seconds and at least one.
#[test]
fn an_empty_profile_parses_its_arguments() {
    let profiler = Profiler::new(ms(33), Arc::new(SourceMeters::default()));
    let info = profiler.profile(Some(0), Some(0), Some(ms(10)));
    assert!(info.drawers.is_empty());
    assert!(info.units.is_empty());
    assert!(!info.thread_cpu);
    assert!(info.process_cpu_pct.is_some());
    let info = profiler.profile(Some(1000), Some(1000), None);
    assert!(info.seconds.get() < 60.0);
}
