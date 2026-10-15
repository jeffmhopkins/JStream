//! Engine scenarios on fake devices and a fake clock.
//!
//! R-RT-004 (`overload_drops_and_counts`): under overload the stages drop and count,
//! and `stats.tick` reports the totals. This drives the stages and fakes directly, and
//! publishes capture, render and output counts by hand beside them. `output.start`
//! registers the same kinds of sources on the live pipeline, where the render thread
//! publishes the capture skips and the render ticks it skipped to keep up with the
//! clock.
//!
//! R-RT-006 (`rt_panic_is_reported`): a panic on the render or mixer thread of a
//! running pipeline is caught at the thread boundary and reported to every client as
//! the output settling `failed`, and the engine keeps serving calls. The render thread
//! composites on the CPU (`Engine::with_cpu_compositor`), so no wgpu adapter is needed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

use jstream_audio::{Mixer, MixerConfig};
use jstream_control::{Control, EventStream};
use jstream_core::{
    AudioBusDrops, AudioInputDrops, DropSnapshot, Engine, RecordingPublish, VideoEncodeDrops,
    audio_bus_drops, audio_input_drops, capture_drops, output_packet_drops, render_tick_drops,
    video_encode_drops,
};
use jstream_encode::{SessionOptions, VideoSession};
use jstream_platform::fake::{
    FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeMediaDecoders,
    FakeVideoDevices,
};
use jstream_platform::media::{
    AudioFormat, ChannelLayout, Codec, ColorInfo, FrameSize, MediaTime, PixelFormat, SampleFormat,
    Timebase, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{
    Backends, Capture, CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo,
    HardwarePreference, MediaClock, PlatformError, RateControl, VideoEncoderConfig, VideoSurface,
};
use jstream_protocol::{
    Action, Batch, Call, CallResult, Edit, Event, Hello, OutputId, OutputState, PROTOCOL_VERSION,
    Rev, SceneId,
};

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("only `video.snapshot` waits, and this test does not call it"),
    }
}

fn drain(events: &mut EventStream) -> Vec<Event> {
    let mut out = Vec::new();
    while let Poll::Ready(Some(event)) = events
        .as_mut()
        .poll_next(&mut Context::from_waker(Waker::noop()))
    {
        out.push(event);
    }
    out
}

fn frame(index: i64) -> VideoSurface {
    let fps = Timebase::new(1, 60).expect("fps");
    VideoFrame::zeroed(VideoFrameInfo {
        size: FrameSize::new(16, 16).expect("size"),
        format: PixelFormat::Nv12,
        color: ColorInfo::default(),
        pts: MediaTime::new(index, fps),
    })
    .expect("frame")
    .into()
}

/// A subscriber that never reads, so the encoder's fan-out drops packets, plus a mixer
/// jumped far enough ahead that it skips frames. Both totals show up in `stats.tick`,
/// summed with the capture, render and output counts published beside them, and a
/// command issued afterwards still runs.
#[test]
fn overload_drops_and_counts() {
    let session = Arc::new(
        VideoSession::open(
            &FakeCodecs,
            VideoEncoderConfig {
                codec: Codec::H264,
                size: FrameSize::new(16, 16).expect("size"),
                input_format: PixelFormat::Nv12,
                frame_period: Timebase::new(1, 60).expect("fps"),
                rate_control: RateControl::Cbr { kbps: 2_500 },
                keyframe_interval: 4,
                hardware: HardwarePreference::Avoid,
            },
            SessionOptions {
                input_capacity: 64,
                subscriber_capacity: 1,
                ..SessionOptions::default()
            },
        )
        .expect("open"),
    );
    let slow = session.subscribe_with_capacity(1);
    let input = session.input();
    for i in 0..8 {
        input
            .push(frame(i))
            .expect("the input queue holds the frame");
    }
    let encoded = session.finish().expect("finish");
    assert!(
        encoded.subscriber_drops > 0,
        "a subscriber that never reads loses packets"
    );
    assert_eq!(
        encoded.dropped_queue_full + encoded.dropped_encoder_busy,
        0,
        "the input queue was larger than the push"
    );

    let mut mixer = Mixer::new(MixerConfig {
        block_frames: 480,
        latency_frames: 480,
        ring_frames: 8192,
        ..MixerConfig::default()
    })
    .expect("mixer");
    let format = AudioFormat::new(
        mixer.config().sample_rate,
        ChannelLayout::Stereo,
        SampleFormat::F32Interleaved,
    )
    .expect("format");
    let (_producer, mic) = mixer.add_input(format).expect("input");
    let mut block = vec![0.0; mixer.config().block_frames * 2];
    let rate = Timebase::per_second(mixer.config().sample_rate).expect("rate");
    mixer
        .poll(MediaTime::new(0, rate), &mut block)
        .expect("first poll");
    // Half a ring behind the first block: the mixer skips ahead instead of stalling.
    let jumped = MediaTime::new(10_000, rate);
    mixer.poll(jumped, &mut block).expect("jumped poll");
    let mic_drops = audio_input_drops(mic.stats());
    let bus_drops = audio_bus_drops(mixer.bus().stats());
    assert_eq!(
        mic_drops.audio_frames, 0,
        "nothing was fed, so nothing was late"
    );
    assert!(
        bus_drops.audio_frames > 0,
        "falling behind the clock skips frames, {}",
        bus_drops.audio_frames
    );

    // What the render thread publishes for a capture stream: the mailbox's skips. The
    // fake generates a frame when it is read, so it never replaces one; a real mailbox
    // does, and this is that counter. The render thread publishes its skipped ticks
    // into the same cell; `render_tick_drops(2)` stands in for them here.
    let capture = capture_drops(CaptureStats {
        frames_captured: 30,
        frames_skipped: 4,
        frames_undecodable: 0,
    });
    let render = render_tick_drops(2);
    let output = output_packet_drops(3);
    let published = DropSnapshot::new();
    published.publish(capture.saturating_add(render).saturating_add(output));

    let engine = Engine::new().with_output(OutputId::new("yt"));
    engine.add_drop_source(VideoEncodeDrops::new(Arc::clone(&session)));
    engine.add_drop_source(AudioInputDrops::new(mic));
    engine.add_drop_source(AudioBusDrops::new(mixer.bus()));
    engine.add_drop_source(published.clone());

    let user = engine.connect();
    block_on(user.hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "user".into(),
    }))
    .expect("user hello");
    let agent = engine.connect();
    block_on(agent.hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "agent".into(),
    }))
    .expect("agent hello");
    let mut user_events = user.events();
    let mut agent_events = agent.events();

    let expected = video_encode_drops(encoded)
        .saturating_add(mic_drops)
        .saturating_add(bus_drops)
        .saturating_add(capture)
        .saturating_add(render)
        .saturating_add(output);
    engine.emit_stats_tick();
    let tick = Event::StatsTick {
        seq: 1,
        drops: expected,
    };
    assert_eq!(drain(&mut user_events), vec![tick.clone()]);
    assert_eq!(drain(&mut agent_events), vec![tick]);

    engine.emit_stats_tick();
    let again = Event::StatsTick {
        seq: 2,
        drops: expected,
    };
    assert_eq!(
        drain(&mut user_events),
        vec![again.clone()],
        "a later tick carries the same running totals"
    );
    assert_eq!(drain(&mut agent_events), vec![again]);

    // The control plane still accepts a command. The tick did not stall it.
    let applied = block_on(user.call(Call::Batch(Batch::single(Edit::SceneCreate {
        scene_id: SceneId::new("main"),
        name: "Main".into(),
    }))))
    .expect("batch");
    assert_eq!(applied.result, CallResult::Batch { rev: Rev(1) });
    assert!(matches!(
        drain(&mut user_events).as_slice(),
        [
            Event::StateChanged { rev: Rev(1), .. },
            Event::StudioChanged { .. }
        ]
    ));

    drop(slow);
}

/// A capture whose stream panics when the render thread reads it, once armed. The read
/// is part of a render tick, so this is a panic inside the tick.
struct PanickingCapture {
    inner: FakeCapture,
    armed: Arc<AtomicBool>,
}

struct PanickingStream {
    inner: Box<dyn CaptureStream>,
    armed: Arc<AtomicBool>,
}

impl Capture for PanickingCapture {
    fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
        self.inner.targets()
    }

    fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
        Ok(Box::new(PanickingStream {
            inner: self.inner.start(target)?,
            armed: Arc::clone(&self.armed),
        }))
    }
}

impl CaptureStream for PanickingStream {
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
        if self.armed.swap(false, Ordering::AcqRel) {
            panic!("scenario: the render tick blew up");
        }
        self.inner.latest()
    }

    fn stats(&self) -> CaptureStats {
        self.inner.stats()
    }
}

/// A clock that panics when the mixer thread reads it, once armed.
struct PanickingMixerClock {
    inner: FakeClock,
    armed: Arc<AtomicBool>,
}

impl MediaClock for PanickingMixerClock {
    fn timebase(&self) -> Timebase {
        self.inner.timebase()
    }

    fn now(&self) -> MediaTime {
        if thread::current().name() == Some("jstream-mixer")
            && self.armed.swap(false, Ordering::AcqRel)
        {
            panic!("scenario: the mixer blew up");
        }
        self.inner.now()
    }
}

fn output_call(start: bool) -> Call {
    let output_id = OutputId::new("yt");
    Call::Action {
        action: if start {
            Action::OutputStart { output_id }
        } else {
            Action::OutputStop { output_id }
        },
    }
}

/// The output states among `events`; `stats.tick` is skipped.
fn output_steps(events: Vec<Event>) -> Vec<OutputState> {
    events
        .into_iter()
        .filter_map(|event| match event {
            Event::OutputStateChanged { state, .. } => Some(state),
            Event::StatsTick { .. } => None,
            other => panic!("unexpected event {other:?}"),
        })
        .collect()
}

/// Feeds the audio inputs and moves the clock on by 50 ms until the output reports
/// a state, for at most ten seconds. The states it reported, in order.
fn pump_until_reported(
    clock: &FakeClock,
    audio: &FakeAudioIo,
    events: &mut EventStream,
) -> Vec<OutputState> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut steps = Vec::new();
    while steps.is_empty() && Instant::now() < deadline {
        audio.deliver(2400, 0.2);
        clock.advance(50_000);
        thread::sleep(Duration::from_millis(10));
        steps = output_steps(drain(events));
    }
    steps
}

/// R-RT-006: a panic inside a render tick, and then one on the mixer thread, are each
/// caught at their thread's boundary instead of killing the thread silently. Both
/// clients see the output settle `failed`, the error event the protocol has for a
/// fatal pipeline error, and the engine still runs a later command.
#[test]
fn rt_panic_is_reported() {
    let clock = FakeClock::new(Timebase::MICROS);
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    let render_armed = Arc::new(AtomicBool::new(false));
    let mixer_armed = Arc::new(AtomicBool::new(false));
    let backends = Backends {
        clock: Arc::new(PanickingMixerClock {
            inner: clock.clone(),
            armed: Arc::clone(&mixer_armed),
        }),
        video_devices: Arc::new(FakeVideoDevices::new(clock.clone())),
        capture: Arc::new(PanickingCapture {
            inner: FakeCapture::new(clock.clone(), FrameSize::new(64, 36).expect("size")),
            armed: Arc::clone(&render_armed),
        }),
        audio: Arc::new(audio.clone()),
        codecs: Arc::new(FakeCodecs),
        gpu: Arc::new(FakeGpuInterop),
        media: Arc::new(FakeMediaDecoders::new()),
        vcam: None,
    };
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends)
        .with_cpu_compositor();

    let user = engine.connect();
    block_on(user.hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "user".into(),
    }))
    .expect("user hello");
    let agent = engine.connect();
    block_on(agent.hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_name: "agent".into(),
    }))
    .expect("agent hello");
    let mut user_events = user.events();
    let mut agent_events = agent.events();

    for (thread_name, armed) in [("render", &render_armed), ("mixer", &mixer_armed)] {
        // Each start consumes its publish target.
        let publish = RecordingPublish::new();
        let _ = engine.clone().with_publish(Box::new(publish.clone()));
        block_on(user.call(output_call(true))).expect("output.start");
        assert_eq!(
            output_steps(drain(&mut agent_events)),
            vec![OutputState::Starting, OutputState::Active]
        );
        drain(&mut user_events);
        // Let media flow before the panic, so it hits a running pipeline.
        let flowing = Instant::now() + Duration::from_secs(10);
        while publish.tags().len() < 4 && Instant::now() < flowing {
            audio.deliver(2400, 0.2);
            clock.advance(50_000);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            publish.tags().len() >= 4,
            "no media flowed before the {thread_name} panic"
        );

        armed.store(true, Ordering::Release);
        let steps = pump_until_reported(&clock, &audio, &mut agent_events);
        assert!(
            !armed.load(Ordering::Acquire),
            "the {thread_name} thread never panicked"
        );
        assert_eq!(
            steps,
            vec![OutputState::Failed],
            "a {thread_name} panic was not reported"
        );
        assert_eq!(
            output_steps(drain(&mut user_events)),
            vec![OutputState::Failed],
            "the other client did not see the {thread_name} panic"
        );
    }

    // The control plane still runs a command after both panics.
    let applied = block_on(agent.call(Call::Batch(Batch::single(Edit::SceneCreate {
        scene_id: SceneId::new("main"),
        name: "Main".into(),
    }))))
    .expect("batch");
    assert_eq!(applied.result, CallResult::Batch { rev: Rev(1) });
}
