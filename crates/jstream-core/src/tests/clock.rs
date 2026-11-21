//! The media clock a client holds and steps to render frame by frame (ADR-0172):
//! `clock.hold`, `clock.step` and `clock.release` through `InProcessControl`.

use super::{block_on, code, connected, output_call};
use crate::Engine;
use jstream_control::Control;
use jstream_media::Timebase;
use jstream_platform::fake::FakeClock;
use jstream_protocol::{Action, Call, OutputId};
use std::sync::Arc;

fn act(control: &dyn Control, action: Action) -> Result<(), Option<String>> {
    block_on(control.call(Call::Action { action }))
        .map(|_| ())
        .map_err(|error| code(&error).map(str::to_owned))
}

fn ms(engine: &Engine) -> i64 {
    engine
        .media_now()
        .rescale(Timebase::MILLIS, jstream_media::Rounding::Nearest)
        .expect("in milliseconds")
        .ticks()
}

/// An engine on a clock the test moves by hand, at 1 s.
fn clocked() -> (Engine, FakeClock) {
    let engine = Engine::new().with_output(OutputId::new("rec"));
    let clock = FakeClock::new(Timebase::MILLIS);
    clock.advance(1_000);
    engine.set_source_clock(Arc::new(clock.clone()));
    (engine, clock)
}

/// Held, the engine's time stands still while real time passes and moves only by its
/// steps; released, it runs with real time again and never backward.
#[test]
fn a_held_clock_moves_only_by_its_steps_and_never_runs_backward() {
    let (engine, clock) = clocked();
    let control = connected(&engine);
    assert_eq!(ms(&engine), 1_000);
    act(&control, Action::ClockHold).expect("hold");
    clock.advance(5_000);
    assert_eq!(ms(&engine), 1_000, "real time passing doesn't move it");
    for _ in 0..3 {
        act(&control, Action::ClockStep { ms: 33 }).expect("step");
    }
    assert_eq!(ms(&engine), 1_099);
    act(&control, Action::ClockRelease).expect("release");
    assert_eq!(ms(&engine), 6_000, "released, it jumps to real time");
    act(&control, Action::ClockRelease).expect("releasing a clock that runs does nothing");

    // Stepped past real time, it waits there for real time to catch up.
    act(&control, Action::ClockHold).expect("hold");
    act(&control, Action::ClockStep { ms: 10_000 }).expect("step");
    act(&control, Action::ClockRelease).expect("release");
    clock.advance(4_000);
    assert_eq!(ms(&engine), 16_000);
    clock.advance(7_000);
    assert_eq!(ms(&engine), 17_000);
}

#[test]
fn a_step_needs_a_held_clock_and_a_size() {
    let (engine, _clock) = clocked();
    let control = connected(&engine);
    assert_eq!(
        act(&control, Action::ClockStep { ms: 33 }),
        Err(Some("action_failed".into())),
        "not held"
    );
    act(&control, Action::ClockHold).expect("hold");
    for ms in [0, 10_001] {
        assert_eq!(
            act(&control, Action::ClockStep { ms }),
            Err(Some("invalid_argument".into())),
            "{ms}"
        );
    }
    act(&control, Action::ClockStep { ms: 10_000 }).expect("the longest step");
}

/// An output follows real time, so the clock isn't held while one runs, and none starts
/// while it is held.
#[test]
fn outputs_and_a_held_clock_exclude_each_other() {
    let (engine, _clock) = clocked();
    let control = connected(&engine);
    act(&control, Action::ClockHold).expect("hold");
    let started = block_on(control.call(output_call("rec", true)));
    assert_eq!(
        started.as_ref().err().and_then(code),
        Some("action_failed"),
        "{started:?}"
    );
    act(&control, Action::ClockRelease).expect("release");
    block_on(control.call(output_call("rec", true))).expect("start");
    assert_eq!(
        act(&control, Action::ClockHold),
        Err(Some("action_failed".into()))
    );
    block_on(control.call(output_call("rec", false))).expect("stop");
    act(&control, Action::ClockHold).expect("hold once it stopped");
}
