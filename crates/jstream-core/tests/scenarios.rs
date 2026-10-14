//! R-RT-004: under overload the stages drop and count, and `stats.tick` reports the
//! totals. This drives the stages and fakes directly, and publishes capture, render
//! and output counts by hand beside them. `output.start` registers the same kinds of
//! sources on the live pipeline, where the render thread publishes the capture skips
//! and the render ticks it skipped to keep up with the clock.

use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use jstream_audio::{Mixer, MixerConfig};
use jstream_control::{Control, EventStream};
use jstream_core::{
    AudioBusDrops, AudioInputDrops, DropSnapshot, Engine, VideoEncodeDrops, audio_bus_drops,
    audio_input_drops, capture_drops, output_packet_drops, render_tick_drops, video_encode_drops,
};
use jstream_encode::{SessionOptions, VideoSession};
use jstream_platform::fake::FakeCodecs;
use jstream_platform::media::{
    AudioFormat, ChannelLayout, Codec, ColorInfo, FrameSize, MediaTime, PixelFormat, SampleFormat,
    Timebase, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{
    CaptureStats, HardwarePreference, RateControl, VideoEncoderConfig, VideoSurface,
};
use jstream_protocol::{
    Batch, Call, CallResult, Edit, Event, Hello, OutputId, PROTOCOL_VERSION, Rev, SceneId,
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
        [Event::StateChanged { rev: Rev(1), .. }]
    ));

    drop(slow);
}
