//! Crate-level tests: every trait is dyn-compatible with the thread bounds the engine
//! needs, and the fake backends implement them through trait objects only.

use std::sync::{Arc, Mutex};

use jstream_media::{
    AudioBuffer, AudioFormat, ChannelLayout, Codec, ColorInfo, FrameSize, MediaTime, Packet,
    PixelFormat, SampleFormat, Timebase, VideoFrame, VideoFrameInfo,
};

use crate::fake::{self, FakeAudioIo, FakeClock, FakeVcamSink};
use crate::{
    AudioEncoder, AudioEncoderConfig, AudioInputHandler, AudioInputTarget, AudioIo,
    AudioOutputHandler, AudioOutputTarget, AudioPacket, AudioProcess, AudioRenderBuffer,
    AudioStream, Backends, Capture, CaptureStream, CaptureTarget, Codecs, DeviceId, FrameSync,
    GpuContext, GpuFrame, GpuInterop, HardwarePreference, MediaClock, PlatformError, RateControl,
    VcamFormat, VcamSink, VideoDecoder, VideoDecoderConfig, VideoEncoder, VideoEncoderConfig,
    VideoSurface,
};

fn assert_send<T: Send + ?Sized>() {}
fn assert_send_sync<T: Send + Sync + ?Sized>() {}

#[test]
fn traits_are_dyn_compatible_with_their_thread_bounds() {
    // Shared backends: one `Arc` used from several threads.
    assert_send_sync::<dyn MediaClock>();
    assert_send_sync::<dyn Capture>();
    assert_send_sync::<dyn AudioIo>();
    assert_send_sync::<dyn Codecs>();
    assert_send_sync::<dyn GpuInterop>();
    assert_send_sync::<dyn FrameSync>();
    // Frames cross threads: capture to render to encode.
    assert_send_sync::<GpuFrame>();
    assert_send_sync::<VideoSurface>();
    assert_send_sync::<GpuContext>();
    // Sessions: moved to the thread that drives them.
    assert_send::<dyn CaptureStream>();
    assert_send::<dyn AudioStream>();
    assert_send::<dyn AudioInputHandler>();
    assert_send::<dyn AudioOutputHandler>();
    assert_send::<dyn VideoEncoder>();
    assert_send::<dyn AudioEncoder>();
    assert_send::<dyn VideoDecoder>();
    assert_send::<dyn VcamSink>();
    assert_send::<Backends>();
    assert_send_sync::<PlatformError>();
}

fn ms(t: i64) -> MediaTime {
    MediaTime::new(t, Timebase::MILLIS)
}

fn setup() -> (FakeClock, Backends) {
    let clock = FakeClock::new(Timebase::MILLIS);
    let backends = fake::backends(&clock).unwrap();
    (clock, backends)
}

#[test]
fn clock_moves_only_when_advanced_and_clones_share_it() {
    let clock = FakeClock::new(Timebase::MILLIS);
    let shared: Arc<dyn MediaClock> = Arc::new(clock.clone());
    assert_eq!(shared.timebase(), Timebase::MILLIS);
    assert_eq!(shared.now(), ms(0));
    clock.advance(16);
    assert_eq!(shared.now(), ms(16));
    clock.advance(u64::MAX);
    assert_eq!(
        shared.now().ticks(),
        i64::MAX,
        "saturates instead of wrapping"
    );
}

#[test]
fn capture_delivers_one_frame_per_clock_step() {
    let (clock, b) = setup();
    let targets = b.capture.targets().unwrap();
    assert_eq!(targets.len(), 1);
    assert!(targets[0].primary);
    let mut stream = b.capture.start(&targets[0].target).unwrap();

    let first = stream.latest().unwrap().expect("a first frame");
    assert_eq!(first.info().pts, ms(0));
    assert_eq!(first.info().format, PixelFormat::Bgra8);
    assert!(
        stream.latest().unwrap().is_none(),
        "no new frame until time moves"
    );
    clock.advance(16);
    assert_eq!(stream.latest().unwrap().unwrap().info().pts, ms(16));
    assert_eq!(stream.stats().frames_captured, 2);
    assert_eq!(stream.stats().frames_skipped, 0);
}

#[test]
fn capture_rejects_unknown_targets() {
    let (_, b) = setup();
    let err = b
        .capture
        .start(&CaptureTarget::Window(DeviceId::new("w1")))
        .err();
    assert_eq!(
        err,
        Some(PlatformError::NotFound {
            kind: "window",
            id: "w1".into()
        })
    );
    assert!(
        b.capture
            .start(&CaptureTarget::Display(DeviceId::new("other")))
            .is_err()
    );
}

/// One recorded packet: device position, time and sample count.
type Seen = (u64, Option<MediaTime>, usize);

/// Records what the audio thread hands it: packets, and how many errors.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Seen>>>, Arc<Mutex<u32>>);

impl AudioInputHandler for Recorder {
    fn on_packet(&mut self, packet: &AudioPacket<'_>) {
        self.0
            .lock()
            .unwrap()
            .push((packet.device_position, packet.time, packet.samples.len()));
    }

    fn on_error(&mut self, _error: &PlatformError) {
        *self.1.lock().unwrap() += 1;
    }
}

struct Tone(f32);

impl AudioOutputHandler for Tone {
    fn render(&mut self, buffer: &mut AudioRenderBuffer<'_>) {
        buffer.samples.fill(self.0);
    }

    fn on_error(&mut self, _error: &PlatformError) {}
}

#[test]
fn audio_input_reports_device_position_and_clock_time() {
    let clock = FakeClock::new(Timebase::MILLIS);
    let io = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).unwrap();
    let shared: Arc<dyn AudioIo> = Arc::new(io.clone());
    assert_eq!(shared.devices().unwrap().len(), 2);

    let rec = Recorder::default();
    let stream = shared
        .open_input(&AudioInputTarget::DefaultInput, Box::new(rec.clone()))
        .unwrap();
    assert_eq!(stream.format().channels(), 2);
    io.deliver(480, 0.5);
    clock.advance(10);
    io.deliver(480, 0.5);
    assert_eq!(
        *rec.0.lock().unwrap(),
        vec![(0, Some(ms(0)), 960), (480, Some(ms(10)), 960)]
    );

    drop(stream);
    assert_eq!(io.open_streams(), 0, "dropping the stream closes it");
    io.deliver(480, 0.5);
    assert_eq!(rec.0.lock().unwrap().len(), 2);
}

#[test]
fn audio_output_is_filled_by_the_handler() {
    let clock = FakeClock::new(Timebase::MILLIS);
    let io = FakeAudioIo::new(clock, 48_000, ChannelLayout::Mono).unwrap();
    let shared: Arc<dyn AudioIo> = Arc::new(io.clone());
    let _stream = shared
        .open_output(&AudioOutputTarget::DefaultOutput, Box::new(Tone(0.25)))
        .unwrap();
    assert_eq!(io.render(4), vec![vec![0.25; 4]]);
}

#[test]
fn audio_unplug_reports_once_and_closes() {
    let clock = FakeClock::new(Timebase::MILLIS);
    let io = FakeAudioIo::new(clock, 48_000, ChannelLayout::Stereo).unwrap();
    let shared: Arc<dyn AudioIo> = Arc::new(io.clone());
    let rec = Recorder::default();
    let _stream = shared
        .open_input(
            &AudioInputTarget::DefaultOutputLoopback,
            Box::new(rec.clone()),
        )
        .unwrap();
    io.unplug();
    assert_eq!(*rec.1.lock().unwrap(), 1);
    assert_eq!(io.open_streams(), 0);
}

#[test]
fn audio_rejects_unknown_targets() {
    let (_, b) = setup();
    for target in [
        AudioInputTarget::ProcessLoopback { pid: 4 },
        AudioInputTarget::Input(DeviceId::new("nope")),
    ] {
        let err = b
            .audio
            .open_input(&target, Box::new(Recorder::default()))
            .err();
        assert!(
            matches!(err, Some(PlatformError::NotFound { .. })),
            "{target:?}"
        );
        let err = b.audio.input_format(&target).err();
        assert!(
            matches!(err, Some(PlatformError::NotFound { .. })),
            "{target:?}"
        );
    }
}

#[test]
fn audio_lists_processes_and_records_a_listed_one() {
    let clock = FakeClock::new(Timebase::MILLIS);
    let io = FakeAudioIo::new(clock, 48_000, ChannelLayout::Stereo).unwrap();
    let shared: Arc<dyn AudioIo> = Arc::new(io.clone());
    assert_eq!(shared.processes().unwrap(), vec![]);
    let game = AudioProcess {
        pid: 40,
        parent_pid: Some(4),
        exe: "game.exe".into(),
    };
    io.set_processes(vec![game.clone()]);
    assert_eq!(shared.processes().unwrap(), vec![game]);
    let target = AudioInputTarget::ProcessLoopback { pid: 40 };
    let format = shared.input_format(&target).unwrap();
    let stream = shared
        .open_input(&target, Box::new(Recorder::default()))
        .unwrap();
    assert_eq!(stream.format(), format);
    assert_eq!(io.input_targets(), vec![target]);
    // The process exits; its stream stays open until the engine drops it.
    io.set_processes(vec![]);
    assert_eq!(io.open_streams(), 1);
    drop(stream);
    assert_eq!(io.input_targets(), vec![]);
}

fn size() -> FrameSize {
    FrameSize::new(64, 36).unwrap()
}

fn nv12(pts: MediaTime) -> VideoSurface {
    VideoFrame::zeroed(VideoFrameInfo {
        size: size(),
        format: PixelFormat::Nv12,
        color: ColorInfo::default(),
        pts,
    })
    .unwrap()
    .into()
}

fn video_config() -> VideoEncoderConfig {
    VideoEncoderConfig {
        codec: Codec::H264,
        size: size(),
        input_format: PixelFormat::Nv12,
        frame_period: Timebase::new(1, 60).unwrap(),
        rate_control: RateControl::Cbr { kbps: 6_000 },
        keyframe_interval: 2,
        hardware: HardwarePreference::Prefer,
    }
}

#[test]
fn video_encoder_keeps_timing_marks_keyframes_and_pushes_back() {
    let (_, b) = setup();
    let mut enc = b.codecs.video_encoder(&video_config()).unwrap();
    assert!(!enc.info().hardware);

    enc.encode(nv12(ms(0))).unwrap();
    enc.encode(nv12(ms(16))).unwrap();
    assert!(!enc.can_accept());
    assert_eq!(
        enc.encode(nv12(ms(33))),
        Err(PlatformError::Busy {
            what: "video encoder"
        })
    );
    let mut out = Vec::new();
    enc.poll(&mut out).unwrap();
    assert!(enc.can_accept());
    enc.encode(nv12(ms(33))).unwrap();
    enc.finish(&mut out).unwrap();

    let summary: Vec<_> = out.iter().map(|p| (p.pts(), p.is_keyframe())).collect();
    assert_eq!(
        summary,
        vec![(ms(0), true), (ms(16), false), (ms(33), true)]
    );
    assert!(
        out.iter()
            .all(|p| p.codec() == Codec::H264 && p.dts() == p.pts())
    );
    assert!(matches!(
        enc.encode(nv12(ms(50))),
        Err(PlatformError::InvalidState { .. })
    ));
}

#[test]
fn video_encoder_forces_a_keyframe_on_request_and_restarts_the_cadence() {
    let (_, b) = setup();
    let mut config = video_config();
    config.keyframe_interval = 4;
    let mut enc = b.codecs.video_encoder(&config).unwrap();
    let mut out = Vec::new();
    for i in 0..8 {
        if i == 2 {
            // Two requests before one frame ask for one keyframe.
            enc.request_keyframe().unwrap();
            enc.request_keyframe().unwrap();
        }
        enc.encode(nv12(ms(i * 16))).unwrap();
        enc.poll(&mut out).unwrap();
    }
    enc.finish(&mut out).unwrap();
    let keyframes: Vec<bool> = out.iter().map(Packet::is_keyframe).collect();
    assert_eq!(
        keyframes,
        [true, false, true, false, false, false, true, false],
        "frame 2 is forced, and the next one comes an interval after it"
    );
    assert!(matches!(
        enc.request_keyframe(),
        Err(PlatformError::InvalidState { .. })
    ));
}

#[test]
fn video_encoder_rejects_mismatched_frames_and_configs() {
    let (_, b) = setup();
    let mut enc = b.codecs.video_encoder(&video_config()).unwrap();
    let bgra: VideoSurface = VideoFrame::zeroed(VideoFrameInfo {
        size: size(),
        format: PixelFormat::Bgra8,
        color: ColorInfo::default(),
        pts: ms(0),
    })
    .unwrap()
    .into();
    assert!(matches!(
        enc.encode(bgra),
        Err(PlatformError::Unsupported { .. })
    ));

    let mut config = video_config();
    config.codec = Codec::Aac;
    assert!(b.codecs.video_encoder(&config).is_err());
    let mut config = video_config();
    config.hardware = HardwarePreference::Require;
    assert!(b.codecs.video_encoder(&config).is_err());
    let mut config = video_config();
    config.keyframe_interval = 0;
    assert!(b.codecs.video_encoder(&config).is_err());
}

#[test]
fn audio_encoder_emits_whole_frames_on_the_sample_clock() {
    let (_, b) = setup();
    let format =
        AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved).unwrap();
    let mut enc = b
        .codecs
        .audio_encoder(&AudioEncoderConfig {
            codec: Codec::Aac,
            input_format: format,
            kbps: 160,
        })
        .unwrap();
    // Starts at 10 ms = sample 480; 1500 frames make one full packet plus a remainder.
    enc.encode(&AudioBuffer::silence(format, ms(10), 1_500).unwrap())
        .unwrap();
    let mut out = Vec::new();
    enc.poll(&mut out).unwrap();
    assert_eq!(out.len(), 1);
    enc.finish(&mut out).unwrap();
    let pts: Vec<_> = out.iter().map(Packet::pts).collect();
    let tb = format.timebase();
    assert_eq!(
        pts,
        vec![MediaTime::new(480, tb), MediaTime::new(480 + 1024, tb)]
    );
    assert!(out.iter().all(Packet::is_keyframe));

    let mono = AudioFormat::new(48_000, ChannelLayout::Mono, SampleFormat::F32Interleaved).unwrap();
    assert!(
        enc.encode(&AudioBuffer::silence(mono, ms(0), 1).unwrap())
            .is_err()
    );
}

#[test]
fn video_decoder_returns_one_frame_per_packet() {
    let (_, b) = setup();
    let mut dec = b
        .codecs
        .video_decoder(&VideoDecoderConfig {
            codec: Codec::H264,
            output_format: PixelFormat::Nv12,
            hardware: HardwarePreference::Prefer,
        })
        .unwrap();
    let packet = Packet::new(Codec::H264, true, ms(40), ms(40), vec![0, 0, 0, 1, 0x65]).unwrap();
    dec.decode(&packet).unwrap();
    let aac = Packet::new(Codec::Aac, true, ms(0), ms(0), vec![1]).unwrap();
    assert!(matches!(
        dec.decode(&aac),
        Err(PlatformError::Unsupported { .. })
    ));
    let mut out = Vec::new();
    dec.finish(&mut out).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].info().pts, ms(40));
    assert_eq!(out[0].info().format, PixelFormat::Nv12);
}

#[test]
fn video_decoder_reorders_frames_into_presentation_order() {
    let (_, b) = setup();
    let mut dec = b
        .codecs
        .video_decoder(&VideoDecoderConfig {
            codec: Codec::H264,
            output_format: PixelFormat::Nv12,
            hardware: HardwarePreference::Prefer,
        })
        .unwrap();
    // I P B B in decode order, presented I B B P: (pts, dts) in ms.
    let packets = [(33, 0), (132, 33), (66, 66), (99, 99)];
    let mut out = Vec::new();
    let mut ready_after_each = Vec::new();
    for (pts, dts) in packets {
        let packet = Packet::new(Codec::H264, pts == 33, ms(pts), ms(dts), vec![0, 0, 0, 1]);
        dec.decode(&packet.unwrap()).unwrap();
        dec.poll(&mut out).unwrap();
        ready_after_each.push(out.len());
    }
    assert_eq!(
        ready_after_each,
        vec![0, 1, 2, 3],
        "each frame waits until no later packet can present before it"
    );
    dec.finish(&mut out).unwrap();
    let pts: Vec<_> = out.iter().map(|f| f.info().pts).collect();
    assert_eq!(pts, vec![ms(33), ms(66), ms(99), ms(132)]);
}

#[test]
fn vcam_sink_needs_start_and_a_matching_format() {
    let fake = FakeVcamSink::new();
    let probe = fake.probe();
    let mut sink: Box<dyn VcamSink> = Box::new(fake);
    assert!(matches!(
        sink.publish(nv12(ms(0))),
        Err(PlatformError::InvalidState { .. })
    ));
    let format = VcamFormat {
        size: size(),
        pixel_format: PixelFormat::Nv12,
        frame_period: Timebase::new(1, 30).unwrap(),
    };
    sink.start(format).unwrap();
    assert!(sink.start(format).is_err(), "already started");
    assert_eq!(sink.format(), Some(format));
    sink.publish(nv12(ms(33))).unwrap();
    assert_eq!(probe.published(), 1);
    assert_eq!(probe.last_pts(), Some(ms(33)));
    sink.stop().unwrap();
    assert_eq!(sink.format(), None);
    sink.stop().unwrap();
}

#[test]
fn dropping_a_started_vcam_sink_stops_it() {
    let fake = FakeVcamSink::new();
    let probe = fake.probe();
    let mut sink: Box<dyn VcamSink> = Box::new(fake);
    let format = VcamFormat {
        size: size(),
        pixel_format: PixelFormat::Nv12,
        frame_period: Timebase::new(1, 30).unwrap(),
    };
    sink.start(format).unwrap();
    sink.publish(nv12(ms(33))).unwrap();
    assert_eq!(probe.format(), Some(format));
    drop(sink);
    assert_eq!(probe.format(), None, "dropping the sink stops the camera");
    assert_eq!(probe.published(), 1, "the probe still reads the record");
    assert_eq!(probe.last_pts(), Some(ms(33)));
}

#[test]
fn surfaces_report_their_frame_info() {
    let s = nv12(ms(7));
    assert_eq!(s.info().size, size());
    let VideoSurface::Cpu(frame) = s else {
        panic!("the fake capture delivers CPU frames");
    };
    assert_eq!(frame.planes().len(), 2);
}

#[test]
fn fake_backends_have_no_gpu_interop() {
    let clock = FakeClock::new(Timebase::NANOS);
    let backends = fake::backends(&clock).expect("backends");
    assert!(matches!(
        backends.gpu.open_device(),
        Err(PlatformError::Unsupported { .. })
    ));
}

#[test]
fn adapter_ids_compare_by_luid_and_vendor() {
    use crate::AdapterId;
    let amd = AdapterId {
        luid: None,
        vendor: 0x1002,
    };
    assert_eq!(
        amd,
        AdapterId {
            luid: None,
            vendor: 0x1002
        }
    );
    assert_ne!(
        amd,
        AdapterId {
            luid: Some(7),
            vendor: 0x1002
        }
    );
}
