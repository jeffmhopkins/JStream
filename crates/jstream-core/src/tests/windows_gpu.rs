//! The GPU capture path on a real Windows machine (R-TEST-004): an output on the WGC
//! capture of the primary display and the platform's DX12 device, composited with no
//! CPU copy of the capture. `#[ignore]`d, because CI machines have no display and no
//! GPU. Run it with the screen changing (a video playing), with
//! `cargo test -p jstream-core --lib windows_gpu -- --ignored --nocapture`.
//!
//! Audio and the encoders are the fakes: this checks the video path only, and the
//! platform's encoders do not take the engine's frames yet.

use std::io::Write;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use jstream_control::Control;
use jstream_media::{ChannelLayout, Timebase};
use jstream_platform::fake::{FakeAudioIo, FakeClock, FakeCodecs};
use jstream_platform::{Backends, Capture};
use jstream_platform_windows::{QpcClock, WgcCapture, WindowsGpu};
use jstream_protocol::{DropCounts, Event, OutputId};

use super::{GPU_OPEN_TIMEOUT, block_on, connected, drain, output_call};
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
    // The fake audio has its own clock, which never moves: the mixer mixes silence.
    let audio_clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(audio_clock, 48_000, ChannelLayout::Stereo).expect("audio");
    let backends = Backends {
        clock: Arc::new(QpcClock::new().expect("QPC clock")),
        capture: Arc::new(capture),
        audio: Arc::new(audio),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(WindowsGpu::new()),
        vcam: None,
    };
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends)
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
