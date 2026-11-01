//! The GPU capture path on a real Windows machine (R-TEST-004): an output on the WGC
//! capture of the primary display and the platform's DX12 device, composited with no
//! CPU copy of the capture, and `capture.targets` naming the displays and windows that
//! `display` and `window` sources capture (C3). Behind `hw-tests` and `#[ignore]`d,
//! because CI machines have no display and no GPU. Run it with the screen changing (a
//! video playing) and a window open (Notepad will do), with
//! `cargo test -p jstream-core --features hw-tests --lib windows_gpu -- --ignored --nocapture`.
//!
//! Audio and the encoders are the fakes: this checks the video path only, and the
//! platform's encoders do not take the engine's frames yet.

use std::io::Write;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use jstream_control::Control;
use jstream_media::{ChannelLayout, Timebase};
use jstream_platform::fake::{FakeAudioIo, FakeClock, FakeCodecs, FakeVideoDevices};
use jstream_platform::{Backends, Capture};
use jstream_platform_windows::{QpcClock, WgcCapture, WindowsGpu};
use jstream_protocol::{CaptureTargetKind, DropCounts, Event, OutputId, Query, QueryResult};
use jstream_sources::CaptureSpec;

use super::{GPU_OPEN_TIMEOUT, block_on, connected, drain, output_call, query};
use crate::runtime::{CaptureBackends, start_capture};
use crate::{Engine, RecordingPublish};

/// How long the output runs.
const RUN: Duration = Duration::from_secs(10);

/// Runs an output for [`RUN`] and reports what it did: the capture size, the GPU
/// frames composited, the readbacks for the encoder, and the drops of the final
/// `stats.tick`. Every tick of the base rate (30 per second) must read back, the
/// render thread must not fall behind the clock, and new capture frames must arrive on
/// the GPU path.
#[test]
#[ignore = "needs a display, Windows.Graphics.Capture and a DX12 GPU; keep the screen changing"]
fn an_output_composites_wgc_frames_on_the_gpu() {
    let capture = WgcCapture::new();
    let primary = capture
        .targets()
        .expect("targets")
        .into_iter()
        .find(|target| target.primary)
        .expect("a primary display");
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends(capture))
        .with_open_timeout(GPU_OPEN_TIMEOUT)
        .with_publish(Box::new(RecordingPublish::new()));
    let control = connected(&engine);
    let mut events = control.events();
    block_on(control.call(output_call("yt", true))).expect("output.start");
    let started = Instant::now();
    thread::sleep(RUN);
    let ran = started.elapsed();
    let stats = engine.output_stats().expect("still running");
    engine.emit_stats_tick();
    let drops = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            Event::StatsTick { drops, .. } => Some(drops),
            _ => None,
        })
        .next_back()
        .unwrap_or_default();
    block_on(control.call(output_call("yt", false))).expect("output.stop");

    let DropCounts {
        capture_frames,
        render_ticks,
        ..
    } = drops;
    let _ = writeln!(
        std::io::stderr(),
        "windows_gpu: {:?} display over {:.1} s: gpu_captures {}, readbacks {}, \
         stats.tick drops capture_frames {capture_frames} render_ticks {render_ticks}, \
         so about {:.0} capture frames per second",
        primary.size,
        ran.as_secs_f64(),
        stats.gpu_captures,
        stats.readbacks,
        (stats.gpu_captures + capture_frames) as f64 / ran.as_secs_f64(),
    );
    assert!(stats.gpu_captures > 0, "no frame came on the GPU path");
    let ticks = (ran.as_secs_f64() * 30.0) as u64;
    assert!(
        stats.readbacks + render_ticks + 30 >= ticks,
        "{} readbacks and {render_ticks} skipped ticks in {ticks} ticks",
        stats.readbacks
    );
    assert!(
        render_ticks * 100 <= ticks,
        "the render thread skipped {render_ticks} of {ticks} ticks"
    );
}

/// WGC capture and the platform's GPU device, with the fakes for audio and the
/// encoders.
fn backends(capture: WgcCapture) -> Backends {
    // The fake audio has its own clock, which never moves: the mixer mixes silence.
    let audio_clock = FakeClock::new(Timebase::MICROS);
    let audio =
        FakeAudioIo::new(audio_clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    Backends {
        clock: Arc::new(QpcClock::new().expect("QPC clock")),
        capture: Arc::new(capture),
        audio: Arc::new(audio),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(WindowsGpu::new()),
        media: Arc::new(jstream_platform::fake::FakeMediaDecoders::new()),
        vcam: None,
        video_devices: Arc::new(FakeVideoDevices::new(audio_clock)),
    }
}

/// `capture.targets` lists the displays, the primary among them, then the open windows,
/// and the IDs it gives are what a `display` and a `window` source capture: each starts
/// a WGC capture that delivers a frame (C3).
#[test]
#[ignore = "needs a display, Windows.Graphics.Capture and an open window"]
fn capture_targets_name_what_wgc_captures() {
    let engine = Engine::new().with_backends(backends(WgcCapture::new()));
    let control = connected(&engine);
    let Ok(QueryResult::CaptureTargets { targets }) = query(&control, Query::CaptureTargets) else {
        panic!("a capture.targets result");
    };
    for target in &targets {
        let _ = writeln!(
            std::io::stderr(),
            "windows_gpu: capture target {:?} {} ({}) {:?}{}",
            target.kind,
            target.target,
            target.name,
            target.size,
            if target.primary { ", primary" } else { "" },
        );
    }
    let display = targets
        .iter()
        .find(|target| target.kind == CaptureTargetKind::Display && target.primary)
        .expect("a primary display");
    let window = targets
        .iter()
        .find(|target| target.kind == CaptureTargetKind::Window)
        .expect("an open window");
    assert!(
        targets
            .iter()
            .position(|target| target.kind == CaptureTargetKind::Window)
            > targets
                .iter()
                .rposition(|target| target.kind == CaptureTargetKind::Display),
        "displays come first"
    );

    let capture = CaptureBackends {
        cpu: Arc::new(WgcCapture::new()),
        gpu: None,
        devices: Arc::new(FakeVideoDevices::new(FakeClock::new(Timebase::MILLIS))),
        gpu_devices: None,
        codecs: Arc::new(FakeCodecs),
    };
    for spec in [
        CaptureSpec::Display(Some(display.target.clone())),
        CaptureSpec::Window(Some(window.target.clone())),
    ] {
        let key = crate::snapshot::CaptureKey {
            spec: spec.clone(),
            source: None,
            show_cursor: true,
        };
        let mut stream = start_capture(&capture, &key).expect("the capture starts");
        let deadline = Instant::now() + Duration::from_secs(5);
        let frame = loop {
            match stream.latest() {
                Ok(Some(frame)) => break frame,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                other => panic!("no frame from {spec:?}: {other:?}"),
            }
        };
        let _ = writeln!(
            std::io::stderr(),
            "windows_gpu: {spec:?} delivered a frame of {:?}",
            frame.info().size
        );
    }
}
