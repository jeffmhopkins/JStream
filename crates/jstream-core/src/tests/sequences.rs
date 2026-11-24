//! Sequences (ADR-0184): a timeline's lane moves a channel from where it is, a trigger
//! plays a timeline when a channel rises, and a delayed play waits and can be cancelled.

use super::{batch, block_on, connected, create, edits, query};
use crate::Engine;
use jstream_control::Control;
use jstream_media::Timebase;
use jstream_platform::fake::FakeClock;
use jstream_protocol::{
    Action, Call, CallResult, ChannelDriver as Driver, ChannelEdge, ChannelInfo, Edit, Finite32,
    LaneCurve, LanePoint, LaneTarget, Query, QueryResult, SceneId, TimelineId, TimelineInfo,
    TimelineLane, TimelineLoop, TimelineTrigger,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn act(control: &dyn Control, action: Action) {
    match block_on(control.call(Call::Action { action })).map(|r| r.result) {
        Ok(CallResult::Action { .. }) => {}
        other => panic!("not an action result: {other:?}"),
    }
}

/// Waits up to three seconds for `done`.
fn until(done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    done()
}

fn level(control: &dyn Control, id: &str) -> Option<(f32, Option<Driver>)> {
    match query(control, Query::ChannelValues).expect("channel.values") {
        QueryResult::ChannelValues { channels } => channels
            .into_iter()
            .find(|c| c.channel_id == id)
            .map(|c| (c.value.get(), c.driver)),
        other => panic!("not channel values: {other:?}"),
    }
}

fn studio(control: &dyn Control) -> (Vec<TimelineId>, Vec<TimelineId>) {
    match query(control, Query::StudioGet).expect("studio.get") {
        QueryResult::StudioGet {
            timelines, waiting, ..
        } => (
            timelines.into_iter().map(|t| t.timeline_id).collect(),
            waiting.into_iter().map(|t| t.timeline_id).collect(),
        ),
        other => panic!("not a studio: {other:?}"),
    }
}

fn timeline(id: &str, duration_ms: u32) -> TimelineInfo {
    TimelineInfo {
        timeline_id: TimelineId::new(id),
        name: String::new(),
        duration_ms,
        looping: TimelineLoop::Once,
        cues: Vec::new(),
        labels: Vec::new(),
        lanes: Vec::new(),
        starts: Vec::new(),
        stops: Vec::new(),
    }
}

fn set_value(channel_id: &str, value: f32) -> Action {
    Action::ChannelSetValue {
        channel_id: channel_id.into(),
        value: Finite32::new(value).expect("finite"),
        fade_ms: None,
    }
}

/// Rule 2: a lane with only a point at 600 ms of 0 starts from the channel's 0.8 and
/// glides down, never jumping, ends at 0 and holds it, driven by the sequence. The lane
/// follows the media clock, so the test steps a fake one and samples after each step:
/// a slow runner can't skip the glide's middle (#1749).
#[test]
fn a_lane_glides_from_the_current_value_and_holds_its_end() {
    let engine = Engine::new();
    let clock = FakeClock::new(Timebase::MILLIS);
    engine.set_source_clock(Arc::new(clock.clone()));
    let control = connected(&engine);
    let mut down = timeline("down", 600);
    down.lanes = vec![TimelineLane {
        target: LaneTarget {
            channel: "lights".into(),
        },
        points: vec![LanePoint {
            at_ms: 600,
            value: Finite32::ZERO,
            curve: LaneCurve::Linear,
        }],
        on: true,
    }];
    batch(
        &control,
        edits(vec![
            create("s"),
            Edit::ChannelSet {
                channel: ChannelInfo::new("lights"),
            },
            Edit::TimelineSet {
                scene_id: SceneId::new("s"),
                timeline: down,
            },
        ]),
    )
    .expect("sets");
    act(&control, set_value("lights", 0.8));
    assert!(until(|| level(&control, "lights").map(|l| l.0) == Some(0.8)));
    act(
        &control,
        Action::TimelinePlay {
            timeline_id: TimelineId::new("down"),
            delay_ms: None,
        },
    );
    // Each 50 ms step of the clock moves the lane; the values tick shows it a moment
    // later, so each sample waits for the value to move (or for the end to hold).
    let mut seen = Vec::new();
    let mut last = level(&control, "lights").map(|l| l.0);
    seen.extend(last);
    for _ in 0..16 {
        clock.advance(50);
        until(|| {
            let now = level(&control, "lights").map(|l| l.0);
            now != last || now == Some(0.0)
        });
        last = level(&control, "lights").map(|l| l.0);
        seen.extend(last);
    }
    assert!(
        seen.first().is_some_and(|v| *v > 0.6),
        "starts near 0.8: {seen:?}"
    );
    // A glide is checked as never going up and passing through several values on its
    // way.
    assert!(
        seen.windows(2).all(|w| w[0] - w[1] > -1e-4),
        "only ever down: {seen:?}"
    );
    let mut between: Vec<f32> = seen
        .iter()
        .copied()
        .filter(|v| *v > 0.01 && *v < 0.79)
        .collect();
    between.dedup();
    assert!(between.len() >= 3, "glides, not jumps: {seen:?}");
    assert!(
        seen.iter().any(|v| *v > 0.1 && *v < 0.7),
        "passes through the middle: {seen:?}"
    );
    assert_eq!(
        level(&control, "lights"),
        Some((
            0.0,
            Some(Driver::Sequence {
                timeline_id: TimelineId::new("down")
            })
        )),
        "holds its last point"
    );
}

/// A channel rising past 0.5 plays a timeline after its delay, which `studio.get` lists
/// as waiting meanwhile; falling stops it; a delayed play is cancelled by a stop.
#[test]
fn a_channel_rising_plays_a_timeline_after_its_delay() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut up = timeline("up", 10_000);
    up.starts = vec![TimelineTrigger::Channel {
        channel_id: "power".into(),
        edge: ChannelEdge::Rises,
        level: None,
        delay_ms: 300,
    }];
    up.stops = vec![TimelineTrigger::Channel {
        channel_id: "power".into(),
        edge: ChannelEdge::Falls,
        level: None,
        delay_ms: 0,
    }];
    batch(
        &control,
        edits(vec![
            create("s"),
            Edit::ChannelSet {
                channel: ChannelInfo::new("power"),
            },
            Edit::TimelineSet {
                scene_id: SceneId::new("s"),
                timeline: up,
            },
            Edit::TimelineSet {
                scene_id: SceneId::new("s"),
                timeline: timeline("later", 10_000),
            },
        ]),
    )
    .expect("sets");
    // The trigger notes where the channel sits on the values thread's first tick after
    // the batch, and a rise needs that note to rise from. A level seen at 0.2 means that
    // tick has run, however slow the runner.
    act(&control, set_value("power", 0.2));
    assert!(
        until(|| level(&control, "power").map(|l| l.0) == Some(0.2)),
        "the values thread ticks"
    );
    assert_eq!(
        studio(&control),
        (Vec::new(), Vec::new()),
        "0.2 is below 0.5"
    );
    let id = TimelineId::new("up");
    act(&control, set_value("power", 1.0));
    assert!(
        until(|| studio(&control).1 == [id.clone()]),
        "waiting: {:?}",
        studio(&control)
    );
    assert!(
        until(|| studio(&control) == (vec![id.clone()], Vec::new())),
        "then playing: {:?}",
        studio(&control)
    );
    act(&control, set_value("power", 0.0));
    assert!(
        until(|| studio(&control).0.is_empty()),
        "falling stops it: {:?}",
        studio(&control)
    );
    let later = TimelineId::new("later");
    act(
        &control,
        Action::TimelinePlay {
            timeline_id: later.clone(),
            delay_ms: Some(5_000),
        },
    );
    assert_eq!(studio(&control).1, std::slice::from_ref(&later));
    act(
        &control,
        Action::TimelineStop {
            timeline_id: later.clone(),
        },
    );
    assert_eq!(
        studio(&control),
        (Vec::new(), Vec::new()),
        "the stop cancels the wait"
    );
}

/// Rule 3: a seek keeps where the lane started from. Seeking a playing "lights down"
/// back to 0 returns the channel to its start value, not to where it had got to.
#[test]
fn a_seek_returns_a_lane_to_where_it_started() {
    let engine = Engine::new();
    let control = connected(&engine);
    let mut down = timeline("down", 3000);
    down.lanes = vec![TimelineLane {
        target: LaneTarget {
            channel: "lights".into(),
        },
        points: vec![LanePoint {
            at_ms: 3000,
            value: Finite32::ZERO,
            curve: LaneCurve::Linear,
        }],
        on: true,
    }];
    batch(
        &control,
        edits(vec![
            create("s"),
            Edit::ChannelSet {
                channel: ChannelInfo::new("lights"),
            },
            Edit::TimelineSet {
                scene_id: SceneId::new("s"),
                timeline: down,
            },
        ]),
    )
    .expect("sets");
    act(&control, set_value("lights", 0.8));
    assert!(until(|| level(&control, "lights").map(|l| l.0) == Some(0.8)));
    act(
        &control,
        Action::TimelinePlay {
            timeline_id: TimelineId::new("down"),
            delay_ms: None,
        },
    );
    assert!(
        until(|| level(&control, "lights").is_some_and(|l| l.0 < 0.6)),
        "the lane moves down"
    );
    act(
        &control,
        Action::TimelineSeek {
            timeline_id: TimelineId::new("down"),
            at_ms: 0,
        },
    );
    // The seek takes effect on a values tick, so wait for one rather than a fixed time.
    // Restarting from where it had got to would glide down from below 0.6, never
    // coming back above 0.7.
    assert!(
        until(|| level(&control, "lights").is_some_and(|l| l.0 > 0.7)),
        "back near the 0.8 it started from, not where it had got to: {:?}",
        level(&control, "lights")
    );
}
