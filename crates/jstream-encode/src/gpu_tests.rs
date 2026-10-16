//! How [`VideoSession::open_on`] picks GPU or CPU frames, and that a GPU session hands
//! every frame back. The GPU frames are textures on a software adapter, so these run on
//! every platform. A machine with no wgpu adapter skips them, except on CI, where that fails.

use std::io::Write;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};

use jstream_platform::fake::FakeCodecs;
use jstream_platform::media::{
    Codec, ColorInfo, FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{
    AdapterId, AudioEncoder, AudioEncoderConfig, CodecInfo, Codecs, FrameSync, GpuContext,
    GpuFrame, HardwarePreference, PlatformError, RateControl, VideoDecoder, VideoDecoderConfig,
    VideoEncoder, VideoEncoderConfig, VideoSurface, wgpu,
};

use crate::{EncoderSettings, SessionOptions, StreamEnd, VideoPath, VideoSession};

fn size() -> FrameSize {
    FrameSize::new(16, 16).unwrap()
}

fn fps() -> Timebase {
    Timebase::new(1, 30).unwrap()
}

/// The engine's CPU configuration: RGBA frames, as the render thread reads them back.
fn config() -> VideoEncoderConfig {
    VideoEncoderConfig {
        codec: Codec::H264,
        size: size(),
        input_format: PixelFormat::Rgba8,
        frame_period: fps(),
        rate_control: RateControl::Cbr { kbps: 2_500 },
        keyframe_interval: 30,
        hardware: HardwarePreference::Avoid,
    }
}

/// Polls a future that native wgpu completes at once.
fn ready<F: Future>(future: F) -> Option<F::Output> {
    match pin!(future)
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// A context on a software adapter (else any adapter), or `None`, said, when the
/// machine has none. CI always has one.
fn context() -> Option<GpuContext> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = [true, false].into_iter().find_map(|fallback| {
        ready(instance.request_adapter(&wgpu::RequestAdapterOptions {
            force_fallback_adapter: fallback,
            ..Default::default()
        }))?
        .ok()
    });
    let Some(adapter) = adapter else {
        assert!(std::env::var_os("CI").is_none(), "CI has no wgpu adapter");
        let _ = writeln!(
            std::io::stderr(),
            "jstream-encode: skipping GPU frame test, no wgpu adapter"
        );
        return None;
    };
    let (device, queue) = ready(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        .expect("native device requests finish at once")
        .expect("a device");
    let id = AdapterId {
        luid: None,
        vendor: 0,
    };
    Some(GpuContext::new(adapter, device, queue, id, None))
}

/// A hold with nothing to wait for.
#[derive(Debug)]
struct NoSync;

impl FrameSync for NoSync {
    fn stage(&self, _queue: &wgpu::Queue) -> Result<(), PlatformError> {
        Ok(())
    }
}

/// An NV12 frame in a texture on `context` with `hold`, at frame `index`. The texture
/// is RGBA: the fake encoder reads only the frame's info.
fn gpu_frame(context: &GpuContext, hold: &Arc<dyn FrameSync>, index: i64) -> VideoSurface {
    let texture = context.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("encode target stand-in"),
        size: wgpu::Extent3d {
            width: size().width(),
            height: size().height(),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let info = VideoFrameInfo {
        size: size(),
        format: PixelFormat::Nv12,
        color: ColorInfo::default(),
        pts: MediaTime::new(index, fps()),
    };
    GpuFrame::new(texture, info, Arc::clone(hold)).into()
}

fn cpu_frame(index: i64) -> VideoSurface {
    VideoFrame::zeroed(VideoFrameInfo {
        size: size(),
        format: PixelFormat::Rgba8,
        color: ColorInfo::default(),
        pts: MediaTime::new(index, fps()),
    })
    .unwrap()
    .into()
}

/// What [`DualCodecs::on_context`] does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GpuMode {
    /// No GPU path (the trait's default).
    None,
    /// Refuses the context.
    Refuse,
    /// A GPU backend whose encoders fail to open.
    FailOpen,
    /// A GPU backend whose encoders take GPU frames only.
    Deliver,
}

/// The fake codecs, with a GPU path that behaves as `mode` says. Counts the GPU
/// encoders opened.
struct DualCodecs {
    mode: GpuMode,
    /// On the GPU backend `on_context` returns.
    gpu: bool,
    opened: Arc<AtomicU64>,
}

impl DualCodecs {
    fn new(mode: GpuMode) -> Self {
        Self {
            mode,
            gpu: false,
            opened: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Codecs for DualCodecs {
    fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
        FakeCodecs.list()
    }

    fn video_encoder(
        &self,
        config: &VideoEncoderConfig,
    ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
        if !self.gpu {
            return FakeCodecs.video_encoder(config);
        }
        if self.mode == GpuMode::FailOpen {
            return Err(PlatformError::Unsupported {
                what: "a hardware encoder on this adapter".into(),
            });
        }
        let inner = FakeCodecs.video_encoder(config)?;
        self.opened.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(GpuOnly(inner)))
    }

    fn audio_encoder(
        &self,
        config: &AudioEncoderConfig,
    ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
        FakeCodecs.audio_encoder(config)
    }

    fn video_decoder(
        &self,
        config: &VideoDecoderConfig,
    ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
        FakeCodecs.video_decoder(config)
    }

    fn on_context(&self, _context: &GpuContext) -> Result<Option<Arc<dyn Codecs>>, PlatformError> {
        match self.mode {
            GpuMode::None => Ok(None),
            GpuMode::Refuse => Err(PlatformError::InvalidState {
                what: "on_context",
                state: "the context was opened by another platform",
            }),
            GpuMode::FailOpen | GpuMode::Deliver => Ok(Some(Arc::new(Self {
                mode: self.mode,
                gpu: true,
                opened: Arc::clone(&self.opened),
            }))),
        }
    }
}

/// An encoder that takes GPU frames only, as the platform's GPU encoders do.
struct GpuOnly(Box<dyn VideoEncoder>);

impl VideoEncoder for GpuOnly {
    fn info(&self) -> &CodecInfo {
        self.0.info()
    }

    fn can_accept(&self) -> bool {
        self.0.can_accept()
    }

    fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        match frame {
            VideoSurface::Gpu(_) => self.0.encode(frame),
            _ => Err(PlatformError::Unsupported {
                what: "a CPU frame on the GPU encoder".into(),
            }),
        }
    }

    fn poll(
        &mut self,
        out: &mut Vec<jstream_platform::media::Packet>,
    ) -> Result<(), PlatformError> {
        self.0.poll(out)
    }

    fn finish(
        &mut self,
        out: &mut Vec<jstream_platform::media::Packet>,
    ) -> Result<(), PlatformError> {
        self.0.finish(out)
    }
}

fn opened_format(session: &VideoSession) -> PixelFormat {
    match &session.info().settings {
        EncoderSettings::Video(config) => config.input_format,
        other => panic!("a video session with {other:?}"),
    }
}

/// Encodes `frames` and returns how many packets came out and how the stream ended.
fn run(session: &VideoSession, frames: Vec<VideoSurface>) -> (usize, StreamEnd) {
    let sub = session.subscribe();
    let input = session.input();
    for frame in frames {
        input.push(frame).expect("the queue has room");
    }
    let finished = session.finish();
    let mut packets = 0;
    loop {
        match sub.try_recv() {
            Ok(Some(_)) => packets += 1,
            Ok(None) => panic!("the stream ended without saying how: {finished:?}"),
            Err(end) => return (packets, end),
        }
    }
}

fn options() -> SessionOptions {
    SessionOptions {
        input_capacity: 16,
        ..SessionOptions::default()
    }
}

#[test]
fn without_a_context_the_session_takes_cpu_frames() {
    let codecs = DualCodecs::new(GpuMode::Deliver);
    let session = VideoSession::open_on(&codecs, None, config(), options()).expect("open");
    assert_eq!(session.path(), &VideoPath::Cpu { fallback: None });
    assert_eq!(opened_format(&session), PixelFormat::Rgba8);
    assert_eq!(codecs.opened.load(Ordering::Relaxed), 0);
    let (packets, end) = run(&session, (0..3).map(cpu_frame).collect());
    assert_eq!((packets, end), (3, StreamEnd::Finished));
}

#[test]
fn a_context_the_codecs_cannot_use_falls_back_to_cpu_frames() {
    let Some(context) = context() else { return };
    for (mode, reason) in [
        (GpuMode::None, "no GPU path"),
        (GpuMode::Refuse, "refused the GPU context"),
        (GpuMode::FailOpen, "no encoder takes GPU frames"),
    ] {
        let codecs = DualCodecs::new(mode);
        let session =
            VideoSession::open_on(&codecs, Some(&context), config(), options()).expect("open");
        match session.path() {
            VideoPath::Cpu {
                fallback: Some(why),
            } => assert!(why.contains(reason), "{mode:?}: {why}"),
            other => panic!("{mode:?}: {other:?}"),
        }
        assert_eq!(opened_format(&session), PixelFormat::Rgba8, "{mode:?}");
        let (packets, end) = run(&session, (0..3).map(cpu_frame).collect());
        assert_eq!((packets, end), (3, StreamEnd::Finished), "{mode:?}");
    }
}

#[test]
fn a_gpu_session_encodes_nv12_targets_and_lets_go_of_each() {
    let Some(context) = context() else { return };
    let codecs = DualCodecs::new(GpuMode::Deliver);
    let session =
        VideoSession::open_on(&codecs, Some(&context), config(), options()).expect("open");
    assert_eq!(session.path(), &VideoPath::Gpu);
    assert_eq!(codecs.opened.load(Ordering::Relaxed), 1);
    // Opened for the NV12 encode targets, whatever the CPU configuration said.
    assert_eq!(opened_format(&session), PixelFormat::Nv12);
    // A ring of two targets, as a render thread would keep.
    let ring: [Arc<dyn FrameSync>; 2] = [Arc::new(NoSync), Arc::new(NoSync)];
    let frames = (0..6)
        .map(|i| gpu_frame(&context, &ring[i as usize % 2], i))
        .collect();
    let (packets, end) = run(&session, frames);
    assert_eq!((packets, end), (6, StreamEnd::Finished));
    // The producer holds the only clone again, so it may write the targets.
    for hold in &ring {
        assert_eq!(Arc::strong_count(hold), 1);
    }
}

#[test]
fn a_gpu_session_fails_on_a_cpu_frame() {
    let Some(context) = context() else { return };
    let codecs = DualCodecs::new(GpuMode::Deliver);
    let session =
        VideoSession::open_on(&codecs, Some(&context), config(), options()).expect("open");
    let (_, end) = run(&session, vec![cpu_frame(0)]);
    assert!(
        matches!(
            end,
            StreamEnd::Failed(crate::EncodeError::Encoder(
                PlatformError::Unsupported { .. }
            ))
        ),
        "{end:?}"
    );
}
