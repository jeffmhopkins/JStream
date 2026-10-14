//! Crate tests. Sessions run on real threads, so every test is made deterministic by
//! construction: input queues larger than what is pushed, encoders gated by the test, or
//! subscribers that read only after `finish`. No test sleeps to wait for a result.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jstream_platform::fake::{FAKE_AAC_FRAME, FakeCodecs};
use jstream_platform::media::{
    AudioBuffer, AudioFormat, ChannelLayout, Codec, ColorInfo, FrameSize, MediaKind, MediaTime,
    Packet, PixelFormat, Rounding, SampleFormat, Timebase, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{
    AudioEncoder, AudioEncoderConfig, CodecInfo, CodecRole, HardwarePreference, PlatformError,
    RateControl, VideoEncoder, VideoEncoderConfig, VideoSurface,
};

use crate::fanout::{FanOut, Sub};
use crate::session::{Counters, Shared};
use crate::{
    AudioInput, AudioSession, EncodeError, EncoderSettings, PushError, SessionKey, SessionOptions,
    SessionRegistry, StreamEnd, StreamInfo, Subscription, VideoInput, VideoSession,
};

const WAIT: Duration = Duration::from_secs(10);

fn fps() -> Timebase {
    Timebase::new(1, 60).unwrap()
}

fn size() -> FrameSize {
    FrameSize::new(16, 16).unwrap()
}

fn video_config(keyframe_interval: u32) -> VideoEncoderConfig {
    VideoEncoderConfig {
        codec: Codec::H264,
        size: size(),
        input_format: PixelFormat::Nv12,
        frame_period: fps(),
        rate_control: RateControl::Cbr { kbps: 2_500 },
        keyframe_interval,
        hardware: HardwarePreference::Avoid,
    }
}

fn frame(index: i64) -> VideoSurface {
    VideoFrame::zeroed(VideoFrameInfo {
        size: size(),
        format: PixelFormat::Nv12,
        color: ColorInfo::default(),
        pts: MediaTime::new(index, fps()),
    })
    .unwrap()
    .into()
}

/// The frame index a packet's pts falls on.
fn index(p: &Packet) -> i64 {
    p.pts().rescale(fps(), Rounding::Nearest).unwrap().ticks()
}

fn options(input_capacity: usize) -> SessionOptions {
    SessionOptions {
        input_capacity,
        poll_interval: Duration::from_millis(1),
        ..SessionOptions::default()
    }
}

fn next(sub: &Subscription) -> Packet {
    sub.recv_timeout(WAIT)
        .expect("the stream is still running")
        .expect("a packet within the timeout")
}

/// Every packet left, then how the stream ended.
fn drain(sub: &Subscription) -> (Vec<Packet>, StreamEnd) {
    let mut packets = Vec::new();
    loop {
        match sub.recv_timeout(WAIT) {
            Ok(Some(p)) => packets.push(p),
            Ok(None) => panic!("the stream neither delivered nor ended within the timeout"),
            Err(end) => return (packets, end),
        }
    }
}

fn push_all(input: &VideoInput, frames: std::ops::Range<i64>) {
    for i in frames {
        input.push(frame(i)).unwrap();
    }
}

// ---------------------------------------------------------------------------------------
// A scripted video encoder: gated input, scripted failures, and a drop probe.

#[derive(Clone, Default)]
struct Script {
    /// `can_accept` is false until this is set (set at construction unless gated).
    open: Arc<AtomicBool>,
    fail_at: Option<u64>,
    panic_at: Option<u64>,
    /// Set when the encoder is dropped.
    dropped: Arc<AtomicBool>,
}

struct ScriptedVideo {
    info: CodecInfo,
    script: Script,
    keyframe_interval: u64,
    frames: u64,
    pending: VecDeque<Packet>,
}

impl ScriptedVideo {
    fn boxed(script: &Script, keyframe_interval: u64) -> Box<dyn VideoEncoder> {
        Box::new(Self {
            info: CodecInfo {
                name: "Scripted H.264".into(),
                codec: Codec::H264,
                role: CodecRole::VideoEncoder,
                hardware: true,
            },
            script: script.clone(),
            keyframe_interval,
            frames: 0,
            pending: VecDeque::new(),
        })
    }
}

impl Drop for ScriptedVideo {
    fn drop(&mut self) {
        self.script.dropped.store(true, Ordering::SeqCst);
    }
}

impl VideoEncoder for ScriptedVideo {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn can_accept(&self) -> bool {
        self.script.open.load(Ordering::SeqCst) && self.pending.len() < 2
    }

    fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        if self.script.panic_at == Some(self.frames) {
            panic!("scripted panic at frame {}", self.frames);
        }
        if self.script.fail_at == Some(self.frames) {
            return Err(PlatformError::Os {
                call: "ScriptedVideo::encode",
                code: Some(0x8000_4005),
                message: "scripted failure".into(),
            });
        }
        let pts = frame.info().pts;
        let keyframe = self.frames.is_multiple_of(self.keyframe_interval);
        let packet = Packet::new(Codec::H264, keyframe, pts, pts, vec![0, 0, 0, 1, 0x65])?;
        self.pending.push_back(packet);
        self.frames += 1;
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        out.extend(self.pending.drain(..));
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.poll(out)
    }
}

fn scripted_session(script: &Script, input_capacity: usize) -> VideoSession {
    VideoSession::start(
        ScriptedVideo::boxed(script, 3),
        video_config(3),
        options(input_capacity),
    )
    .unwrap()
}

fn open_script() -> Script {
    let script = Script::default();
    script.open.store(true, Ordering::SeqCst);
    script
}

// ---------------------------------------------------------------------------------------
// Thread bounds

#[test]
fn handles_have_the_thread_bounds_the_engine_needs() {
    fn send_sync<T: Send + Sync>() {}
    fn send<T: Send>() {}
    send_sync::<VideoSession>();
    send_sync::<AudioSession>();
    send_sync::<VideoInput>();
    send_sync::<AudioInput>();
    send::<Subscription>();
    send_sync::<EncodeError>();
}

// ---------------------------------------------------------------------------------------
// Video

#[test]
fn video_packets_keep_order_and_pts_and_finish_drains() {
    let session = VideoSession::open(&FakeCodecs, video_config(3), options(64)).unwrap();
    let sub = session.subscribe();
    assert_eq!(sub.info().codec.codec, Codec::H264);
    assert_eq!(sub.info().settings, EncoderSettings::Video(video_config(3)));
    push_all(&session.input(), 0..20);
    let stats = session.finish().unwrap();

    let (packets, end) = drain(&sub);
    assert_eq!(end, StreamEnd::Finished);
    let indices: Vec<i64> = packets.iter().map(index).collect();
    assert_eq!(indices, (0..20).collect::<Vec<_>>());
    for p in &packets {
        assert_eq!(p.is_keyframe(), index(p) % 3 == 0, "keyframe pattern");
        assert_eq!(p.pts(), p.dts());
    }
    assert_eq!(stats.received, 20);
    assert_eq!(stats.encoded, 20);
    assert_eq!(stats.packets, 20);
    assert_eq!(stats.keyframes, 7);
    assert_eq!(stats.dropped_queue_full + stats.dropped_encoder_busy, 0);
    assert_eq!(stats.subscriber_drops, 0);
    assert_eq!(sub.stats().delivered, 20);

    // Finishing is idempotent, and a finished session refuses input.
    assert_eq!(session.finish(), Ok(stats));
    assert_eq!(session.input().push(frame(20)), Err(PushError::Closed));
}

#[test]
fn a_full_input_queue_drops_and_counts() {
    let script = Script::default(); // gated: the encoder takes nothing yet
    let session = scripted_session(&script, 4);
    let sub = session.subscribe();
    let input = session.input();
    let results: Vec<_> = (0..10).map(|i| input.push(frame(i))).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 4);
    assert!(results[4..].iter().all(|r| *r == Err(PushError::Full)));
    let stats = session.stats();
    assert_eq!((stats.received, stats.dropped_queue_full), (4, 6));
    assert_eq!(stats.encoded, 0, "a gated encoder takes no input");

    script.open.store(true, Ordering::SeqCst);
    let stats = session.finish().unwrap();
    let (packets, end) = drain(&sub);
    assert_eq!(end, StreamEnd::Finished);
    assert_eq!(packets.iter().map(index).collect::<Vec<_>>(), [0, 1, 2, 3]);
    assert_eq!(stats.encoded, 4);
    assert_eq!(stats.dropped_queue_full, 6);
}

#[test]
fn finish_drops_and_counts_what_a_stuck_encoder_never_takes() {
    let script = Script::default(); // never opened
    let session = VideoSession::start(
        ScriptedVideo::boxed(&script, 3),
        video_config(3),
        SessionOptions {
            finish_timeout: Duration::from_millis(20),
            ..options(4)
        },
    )
    .unwrap();
    let sub = session.subscribe();
    push_all(&session.input(), 0..3);
    let stats = session.finish().unwrap();
    assert_eq!(stats.dropped_encoder_busy, 3);
    assert_eq!(stats.encoded, 0);
    assert_eq!(drain(&sub), (vec![], StreamEnd::Finished));
}

#[test]
fn a_slow_subscriber_drops_without_affecting_a_fast_one() {
    let session = VideoSession::open(&FakeCodecs, video_config(4), options(64)).unwrap();
    let slow = session.subscribe_with_capacity(2);
    let fast = session.subscribe_with_capacity(64);
    push_all(&session.input(), 0..12);
    let stats = session.finish().unwrap();

    // The slow subscriber read nothing while the session ran.
    let (fast_packets, fast_end) = drain(&fast);
    assert_eq!(fast_end, StreamEnd::Finished);
    assert_eq!(
        fast_packets.iter().map(index).collect::<Vec<_>>(),
        (0..12).collect::<Vec<_>>()
    );
    assert_eq!(fast.stats().dropped, 0);

    let (slow_packets, slow_end) = drain(&slow);
    assert_eq!(slow_end, StreamEnd::Finished);
    assert_eq!(slow_packets.iter().map(index).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(slow.stats().delivered, 2);
    assert_eq!(slow.stats().dropped, 10);
    assert_eq!(stats.subscriber_drops, 10);
    assert_eq!(stats.packets, 12, "the encoder was never held back");
}

#[test]
fn a_late_subscriber_starts_at_the_next_keyframe() {
    let session = VideoSession::open(&FakeCodecs, video_config(3), options(64)).unwrap();
    let early = session.subscribe();
    let input = session.input();
    push_all(&input, 0..5);
    for i in 0..5 {
        assert_eq!(index(&next(&early)), i);
    }
    // Packets 0..5 are published. The late subscriber is admitted before packet 5 is.
    let late = session.subscribe();
    assert_eq!(late.info(), session.info());
    push_all(&input, 5..10);
    session.finish().unwrap();

    let (packets, end) = drain(&late);
    assert_eq!(end, StreamEnd::Finished);
    assert_eq!(packets.iter().map(index).collect::<Vec<_>>(), [6, 7, 8, 9]);
    assert!(packets[0].is_keyframe());
    assert_eq!(
        late.stats().dropped,
        0,
        "skipping to the first keyframe isn't a drop"
    );
    assert_eq!(drain(&early).0.len(), 5);
}

#[test]
fn subscribing_after_the_end_reports_the_end() {
    let session = VideoSession::open(&FakeCodecs, video_config(3), options(8)).unwrap();
    session.finish().unwrap();
    let sub = session.subscribe();
    assert_eq!(sub.recv(), Err(StreamEnd::Finished));
    assert_eq!(sub.try_recv(), Err(StreamEnd::Finished));
}

#[test]
fn an_encoder_error_reaches_every_subscriber() {
    let script = Script {
        fail_at: Some(3),
        ..open_script()
    };
    let session = scripted_session(&script, 64);
    let a = session.subscribe();
    let b = session.subscribe();
    let input = session.input();
    for i in 0..6 {
        // Pushes after the failure may be refused; that is the point.
        let _ = input.push(frame(i));
    }
    let expected = EncodeError::Encoder(PlatformError::Os {
        call: "ScriptedVideo::encode",
        code: Some(0x8000_4005),
        message: "scripted failure".into(),
    });
    for sub in [&a, &b] {
        let (packets, end) = drain(sub);
        assert_eq!(packets.iter().map(index).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(end, StreamEnd::Failed(expected.clone()));
    }
    assert_eq!(session.finish(), Err(expected));
    assert_eq!(input.push(frame(9)), Err(PushError::Closed));
    assert!(script.dropped.load(Ordering::SeqCst));
}

#[test]
fn a_panic_on_the_encode_thread_is_caught_and_reported() {
    let script = Script {
        panic_at: Some(1),
        ..open_script()
    };
    let session = scripted_session(&script, 64);
    let sub = session.subscribe();
    push_all(&session.input(), 0..2);
    let (packets, end) = drain(&sub);
    assert_eq!(packets.iter().map(index).collect::<Vec<_>>(), [0]);
    let StreamEnd::Failed(EncodeError::Panicked { message }) = end else {
        panic!("expected a caught panic, got {end:?}");
    };
    assert_eq!(message, "scripted panic at frame 1");
    assert!(matches!(
        session.finish(),
        Err(EncodeError::Panicked { .. })
    ));
}

#[test]
fn dropping_a_session_closes_streams_and_joins_its_thread() {
    let script = open_script();
    let session = scripted_session(&script, 8);
    let sub = session.subscribe();
    let input = session.input();
    input.push(frame(0)).unwrap();
    assert_eq!(index(&next(&sub)), 0);
    drop(session);
    // The encoder is dropped on the encode thread, as its last act; `drop` joined it.
    assert!(script.dropped.load(Ordering::SeqCst));
    assert_eq!(sub.recv(), Err(StreamEnd::Closed));
    assert_eq!(input.push(frame(1)), Err(PushError::Closed));
}

#[test]
fn opening_checks_the_media_kind() {
    let mut config = video_config(3);
    config.codec = Codec::Aac;
    assert_eq!(
        VideoSession::open(&FakeCodecs, config, SessionOptions::default()).err(),
        Some(EncodeError::WrongKind {
            codec: Codec::Aac,
            expected: MediaKind::Video
        })
    );
    let mut config = video_config(3);
    config.hardware = HardwarePreference::Require; // the fakes have no hardware
    assert!(matches!(
        VideoSession::open(&FakeCodecs, config, SessionOptions::default()),
        Err(EncodeError::Open(PlatformError::Unsupported { .. }))
    ));
    let audio = AudioEncoderConfig {
        codec: Codec::H264,
        ..audio_config()
    };
    assert!(matches!(
        AudioSession::open(&FakeCodecs, audio, SessionOptions::default()),
        Err(EncodeError::WrongKind {
            expected: MediaKind::Audio,
            ..
        })
    ));
}

// ---------------------------------------------------------------------------------------
// Fan-out, driven directly

fn packet(i: i64, keyframe: bool) -> Packet {
    let t = MediaTime::new(i, fps());
    Packet::new(Codec::H264, keyframe, t, t, vec![1]).unwrap()
}

#[test]
fn a_subscriber_that_drops_resumes_at_the_next_keyframe() {
    let counters = Counters::default();
    let (joins_tx, joins) = std::sync::mpsc::channel();
    let shared = Shared::for_tests(StreamInfo {
        codec: CodecInfo {
            name: "test".into(),
            codec: Codec::H264,
            role: CodecRole::VideoEncoder,
            hardware: false,
        },
        settings: EncoderSettings::Video(video_config(3)),
    });
    let (sub, subscription) = Sub::new(2, shared);
    joins_tx.send(sub).unwrap();
    let mut fan = FanOut::default();
    fan.admit(&joins, &counters);

    let mut batch = vec![
        packet(0, false),
        packet(1, true),
        packet(2, false),
        packet(3, false),
    ];
    fan.publish(&mut batch, &counters);
    assert!(batch.is_empty());
    // 0 was skipped while joining (not a drop), 1 and 2 delivered, 3 dropped.
    assert_eq!(index(&subscription.try_recv().unwrap().unwrap()), 1);
    assert_eq!(index(&subscription.try_recv().unwrap().unwrap()), 2);
    assert_eq!(subscription.try_recv(), Ok(None));
    assert_eq!(subscription.stats().dropped, 1);

    // Room again, but 4 can't be decoded without 3: skipped and counted until 5.
    let mut batch = vec![packet(4, false), packet(5, true), packet(6, false)];
    fan.publish(&mut batch, &counters);
    assert_eq!(index(&subscription.try_recv().unwrap().unwrap()), 5);
    assert_eq!(index(&subscription.try_recv().unwrap().unwrap()), 6);
    let stats = subscription.stats();
    assert_eq!((stats.delivered, stats.dropped), (4, 2));
    let c = counters.snapshot();
    assert_eq!((c.packets, c.keyframes, c.subscriber_drops), (7, 2, 2));

    // A dropped subscription is removed on the next packet.
    drop(subscription);
    fan.publish(&mut vec![packet(7, true)], &counters);
    assert_eq!(counters.snapshot().subscribers, 0);
}

// ---------------------------------------------------------------------------------------
// Audio

fn audio_format() -> AudioFormat {
    AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved).unwrap()
}

fn audio_config() -> AudioEncoderConfig {
    AudioEncoderConfig {
        codec: Codec::Aac,
        input_format: audio_format(),
        kbps: 160,
    }
}

/// `frames` frames, every sample `value`, starting at sample `start`.
fn block(start: i64, frames: usize, value: f32) -> AudioBuffer {
    let fmt = audio_format();
    let pts = MediaTime::new(start, fmt.timebase());
    AudioBuffer::new(fmt, pts, vec![value; frames * fmt.channels()]).unwrap()
}

#[test]
fn audio_packets_keep_sample_timing_and_finish_pads_the_tail() {
    let session = AudioSession::open(&FakeCodecs, audio_config(), options(64)).unwrap();
    let sub = session.subscribe();
    let input = session.input();
    for k in 0..10 {
        input.push(block(480 * k, 480, 0.5)).unwrap();
    }
    let stats = session.finish().unwrap();
    let (packets, end) = drain(&sub);
    assert_eq!(end, StreamEnd::Finished);
    let frame = i64::try_from(FAKE_AAC_FRAME).unwrap();
    let starts: Vec<i64> = packets.iter().map(|p| p.pts().ticks()).collect();
    // 4800 frames: four whole access units plus a padded fifth.
    assert_eq!(starts, (0..5).map(|k| k * frame).collect::<Vec<_>>());
    assert!(packets.iter().all(Packet::is_keyframe));
    assert_eq!((stats.received, stats.encoded, stats.packets), (10, 10, 5));
    assert_eq!(stats.silence_frames, 0);
}

/// Records every block it is given. Its first `encode` waits for the test's go-ahead,
/// and each `encode` reports that it started.
struct RecordingAudio {
    info: CodecInfo,
    log: Arc<Mutex<Vec<(i64, usize, bool)>>>,
    started: Sender<()>,
    release: Option<Receiver<()>>,
}

impl AudioEncoder for RecordingAudio {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn encode(&mut self, buffer: &AudioBuffer) -> Result<(), PlatformError> {
        let _ = self.started.send(());
        if let Some(release) = self.release.take() {
            let _ = release.recv();
        }
        let silent = buffer.samples().iter().all(|s| *s == 0.0);
        self.log
            .lock()
            .unwrap()
            .push((buffer.pts().ticks(), buffer.frames(), silent));
        Ok(())
    }

    fn poll(&mut self, _out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        Ok(())
    }

    fn finish(&mut self, _out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        Ok(())
    }
}

#[test]
fn dropped_audio_blocks_become_silence_so_later_blocks_keep_their_time() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (started_tx, started) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let encoder = Box::new(RecordingAudio {
        info: CodecInfo {
            name: "Recording AAC".into(),
            codec: Codec::Aac,
            role: CodecRole::AudioEncoder,
            hardware: false,
        },
        log: Arc::clone(&log),
        started: started_tx,
        release: Some(release_rx),
    });
    let session = AudioSession::start(encoder, audio_config(), options(1)).unwrap();
    let input = session.input();

    input.push(block(0, 480, 1.0)).unwrap();
    started.recv_timeout(WAIT).unwrap(); // the encoder holds block 0
    input.push(block(480, 480, 1.0)).unwrap(); // queued
    assert_eq!(input.push(block(960, 480, 1.0)), Err(PushError::Full));
    assert_eq!(input.push(block(1440, 480, 1.0)), Err(PushError::Full));
    release.send(()).unwrap();
    started.recv_timeout(WAIT).unwrap(); // block 480 taken; the queue is empty
    input.push(block(1920, 480, 1.0)).unwrap();
    let stats = session.finish().unwrap();

    assert_eq!(
        *log.lock().unwrap(),
        [
            (0, 480, false),
            (480, 480, false),
            (960, 960, true),
            (1920, 480, false)
        ]
    );
    assert_eq!(stats.dropped_queue_full, 2);
    assert_eq!(stats.silence_frames, 960);
    assert_eq!(stats.encoded, 3);
}

#[test]
fn audio_gaps_without_a_drop_or_beyond_the_limit_are_not_filled() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (started_tx, _started) = std::sync::mpsc::channel();
    let encoder = Box::new(RecordingAudio {
        info: CodecInfo {
            name: "Recording AAC".into(),
            codec: Codec::Aac,
            role: CodecRole::AudioEncoder,
            hardware: false,
        },
        log: Arc::clone(&log),
        started: started_tx,
        release: None,
    });
    let session = AudioSession::start(encoder, audio_config(), options(8)).unwrap();
    let input = session.input();
    input.push(block(0, 480, 1.0)).unwrap();
    input.push(block(4_800, 480, 1.0)).unwrap(); // a gap, but nothing was dropped
    session.finish().unwrap();
    assert_eq!(*log.lock().unwrap(), [(0, 480, false), (4_800, 480, false)]);
}

// ---------------------------------------------------------------------------------------
// Registry

#[test]
fn the_registry_shares_sessions_by_key_while_they_live() {
    let mut registry: SessionRegistry<SessionKey<&str>, VideoSession> = SessionRegistry::new();
    let key = |mix, kf| SessionKey {
        mix,
        settings: EncoderSettings::Video(video_config(kf)),
    };
    let opens = std::cell::Cell::new(0);
    let open = |kf| {
        opens.set(opens.get() + 1);
        VideoSession::open(&FakeCodecs, video_config(kf), options(8))
    };

    let a = registry.get_or_open(key("program", 3), || open(3)).unwrap();
    let b = registry.get_or_open(key("program", 3), || open(3)).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    let c = registry.get_or_open(key("program", 6), || open(6)).unwrap();
    let d = registry.get_or_open(key("preview", 3), || open(3)).unwrap();
    assert!(!Arc::ptr_eq(&a, &c) && !Arc::ptr_eq(&a, &d));
    assert_eq!((opens.get(), registry.len()), (3, 3));

    // The last holder finishes the session; the registry then forgets it.
    drop(b);
    let last = Arc::into_inner(a).expect("the only holder left");
    last.finish().unwrap();
    drop(last);
    assert_eq!(registry.len(), 2);
    assert!(registry.get(&key("program", 3)).is_none());
    let e = registry.get_or_open(key("program", 3), || open(3)).unwrap();
    assert_eq!(opens.get(), 4);

    // A failed open registers nothing.
    let failed: Result<_, EncodeError> = registry.get_or_open(key("broken", 3), || {
        Err(EncodeError::Spawn {
            message: "test".into(),
        })
    });
    assert!(failed.is_err());
    assert_eq!(registry.len(), 3);
    drop((c, d, e));
    assert!(registry.is_empty());
}
