//! R-RT-005: what an encoder session adds around its encoder. Run on the machine the
//! numbers are for, and state that machine with them:
//!
//! ```text
//! cargo bench -p jstream-encode --bench session
//! ```
//!
//! The encoder is `jstream-platform`'s fake, which only stamps a few bytes per input, so
//! these numbers are the session's own cost (the bounded input queue, the encode thread's
//! wake-up, and the fan-out to each subscriber's queue), never an encoder's. A real
//! encoder's cost is measured on the hardware it runs on.
//!
//! - `video_round_trip`: one 1920x1080 NV12 frame from `VideoInput::push` until every
//!   subscriber has received its packet, for 1 and 4 subscribers. The frame is allocated
//!   outside the timing.
//! - `audio_round_trip`: one 1024-frame stereo block from `AudioInput::push` until the
//!   subscriber has received its AAC access unit.

use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_main};
use jstream_encode::{AudioSession, SessionOptions, Subscription, VideoSession};
use jstream_platform::fake::{FAKE_AAC_FRAME, FakeCodecs};
use jstream_platform::media::{
    AudioBuffer, AudioFormat, ChannelLayout, Codec, ColorInfo, FrameSize, MediaTime, Packet,
    PixelFormat, SampleFormat, Timebase, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{
    AudioEncoderConfig, HardwarePreference, RateControl, VideoEncoderConfig, VideoSurface,
};

const WAIT: Duration = Duration::from_secs(10);

fn fps() -> Timebase {
    Timebase::new(1, 60).expect("a valid timebase")
}

fn size() -> FrameSize {
    FrameSize::new(1920, 1080).expect("a non-zero size")
}

fn options() -> SessionOptions {
    SessionOptions {
        input_capacity: 8,
        ..SessionOptions::default()
    }
}

fn frame(index: i64) -> VideoSurface {
    VideoFrame::zeroed(VideoFrameInfo {
        size: size(),
        format: PixelFormat::Nv12,
        color: ColorInfo::default(),
        pts: MediaTime::new(index, fps()),
    })
    .expect("a valid frame")
    .into()
}

fn next(sub: &Subscription) -> Packet {
    sub.recv_timeout(WAIT)
        .expect("the stream is still running")
        .expect("a packet within the timeout")
}

fn video_round_trip(c: &mut Criterion) {
    let mut group = c.benchmark_group("video_round_trip");
    for subscribers in [1, 4] {
        group.bench_function(BenchmarkId::new("subscribers", subscribers), |b| {
            let config = VideoEncoderConfig {
                codec: Codec::H264,
                size: size(),
                input_format: PixelFormat::Nv12,
                frame_period: fps(),
                rate_control: RateControl::Cbr { kbps: 6_000 },
                keyframe_interval: 120,
                hardware: HardwarePreference::Avoid,
            };
            let session =
                VideoSession::open(&FakeCodecs, config, options()).expect("the fake opens");
            let subs: Vec<Subscription> = (0..subscribers).map(|_| session.subscribe()).collect();
            let input = session.input();
            let mut index = 0;
            b.iter_batched(
                || {
                    index += 1;
                    frame(index)
                },
                |frame| {
                    input.push(frame).expect("the queue has room");
                    for sub in &subs {
                        black_box(next(sub));
                    }
                },
                BatchSize::PerIteration,
            );
            session.finish().expect("the session finishes");
        });
    }
    group.finish();
}

fn audio_round_trip(c: &mut Criterion) {
    let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
        .expect("a valid format");
    let config = AudioEncoderConfig {
        codec: Codec::Aac,
        input_format: format,
        kbps: 160,
    };
    let mut group = c.benchmark_group("audio_round_trip");
    group.bench_function("stereo_block", |b| {
        let session = AudioSession::open(&FakeCodecs, config, options()).expect("the fake opens");
        let sub = session.subscribe();
        let input = session.input();
        let mut start = 0;
        b.iter_batched(
            || {
                let pts = MediaTime::new(start, format.timebase());
                start += FAKE_AAC_FRAME as i64;
                AudioBuffer::new(format, pts, vec![0.25; FAKE_AAC_FRAME * format.channels()])
                    .expect("a valid buffer")
            },
            |block| {
                input.push(block).expect("the queue has room");
                black_box(next(&sub));
            },
            BatchSize::PerIteration,
        );
        session.finish().expect("the session finishes");
    });
    group.finish();
}

// `criterion_group!` would generate an undocumented `pub fn` (`missing_docs`).
fn benches() {
    let mut c = Criterion::default().configure_from_args();
    video_round_trip(&mut c);
    audio_round_trip(&mut c);
}

criterion_main!(benches);
