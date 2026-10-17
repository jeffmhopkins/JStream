//! The render thread's GPU capture path and its fallbacks (ARCH › GPU pipeline, step 6),
//! on a software device: which path `open_capture` takes for each kind of platform, and
//! an output whose capture frames are textures composited with no CPU copy.
//!
//! A software adapter stands in for the platform's media adapter, and a fake capture
//! backend delivers `GpuFrame`s on it. A machine with no adapter skips; CI, which
//! installs one, fails instead.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use jstream_control::Control;
use jstream_media::{
    ChannelLayout, ColorInfo, ColorRange, FrameSize, MediaTime, PixelFormat, Timebase,
    VideoFrameInfo,
};
use jstream_platform::fake::{
    FakeAudioIo, FakeCapture, FakeClock, FakeCodecs, FakeGpuInterop, FakeVideoDevices,
};
use jstream_platform::wgpu;
use jstream_platform::{
    AdapterId, Backends, Capture, CaptureStats, CaptureStream, CaptureTarget, CaptureTargetInfo,
    FrameSync, GpuContext, GpuFrame, GpuInterop, MediaClock, PlatformError, VideoSurface,
};
use jstream_protocol::OutputId;
use jstream_render::{Compositor, OpenOptions};

use super::{GPU_OPEN_TIMEOUT, block_on, connected, one_gpu_test_at_a_time, output_call};
use crate::audio_sources::Wanted;
use crate::runtime::{DrawFn, OpenDraw, RenderHub, gpu_draw, open_capture, parts_from};
use crate::{Engine, RecordingPublish};

/// The colour every GPU capture frame is.
const RED: [u8; 4] = [255, 0, 0, 255];

/// GPU interop on a software device, standing in for the platform's.
struct SoftwareGpu;

impl GpuInterop for SoftwareGpu {
    fn open_device(&self) -> Result<GpuContext, PlatformError> {
        let compositor = Compositor::open(OpenOptions::headless()).map_err(|err| {
            PlatformError::Unsupported {
                what: format!("a software adapter: {err}"),
            }
        })?;
        Ok(GpuContext::new(
            compositor.adapter().clone(),
            compositor.device().clone(),
            compositor.queue().clone(),
            AdapterId {
                luid: None,
                vendor: 0,
            },
            None,
        ))
    }

    fn encode_target(
        &self,
        _context: &GpuContext,
        _size: FrameSize,
    ) -> Result<GpuFrame, PlatformError> {
        Err(PlatformError::Unsupported {
            what: "encode targets on the software stand-in".into(),
        })
    }
}

/// What [`DualCapture::on_context`] does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GpuMode {
    /// No GPU path (the trait's default).
    None,
    /// Refuses the context.
    Refuse,
    /// Gives a backend whose captures don't start.
    FailStart,
    /// Gives a backend that delivers [`RED`] GPU frames.
    Deliver,
}

/// The fake display on the CPU path, and on a context per its [`GpuMode`].
struct DualCapture {
    cpu: FakeCapture,
    clock: FakeClock,
    size: FrameSize,
    mode: GpuMode,
}

impl Capture for DualCapture {
    fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
        self.cpu.targets()
    }

    fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
        self.cpu.start(target)
    }

    fn on_context(&self, context: &GpuContext) -> Result<Option<Arc<dyn Capture>>, PlatformError> {
        let gpu = || GpuCapture {
            cpu: FakeCapture::new(self.clock.clone(), self.size),
            clock: self.clock.clone(),
            size: self.size,
            device: context.device().clone(),
            queue: context.queue().clone(),
            fail: self.mode == GpuMode::FailStart,
        };
        match self.mode {
            GpuMode::None => Ok(None),
            GpuMode::Refuse => Err(PlatformError::InvalidState {
                what: "on_context",
                state: "the test refuses every context",
            }),
            GpuMode::FailStart | GpuMode::Deliver => Ok(Some(Arc::new(gpu()))),
        }
    }
}

/// The fake display on a context's device.
struct GpuCapture {
    cpu: FakeCapture,
    clock: FakeClock,
    size: FrameSize,
    device: wgpu::Device,
    queue: wgpu::Queue,
    fail: bool,
}

impl Capture for GpuCapture {
    fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
        self.cpu.targets()
    }

    fn start(&self, _target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
        if self.fail {
            return Err(PlatformError::Unsupported {
                what: "GPU capture in this test".into(),
            });
        }
        Ok(Box::new(GpuStream {
            texture: red_texture(&self.device, &self.queue, self.size),
            clock: self.clock.clone(),
            size: self.size,
            last: None,
            stats: CaptureStats::default(),
            stages: Arc::new(AtomicU64::new(0)),
        }))
    }
}

/// A [`RED`] `Rgba8UnormSrgb` texture of `size`, as a capture ring's slot would be.
fn red_texture(device: &wgpu::Device, queue: &wgpu::Queue, size: FrameSize) -> wgpu::Texture {
    let extent = wgpu::Extent3d {
        width: size.width(),
        height: size.height(),
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("jstream-core-test-capture"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let pixels: Vec<u8> = std::iter::repeat_n(RED, (size.width() * size.height()) as usize)
        .flatten()
        .collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.width() * 4),
            rows_per_image: Some(size.height()),
        },
        extent,
    );
    texture
}

/// Delivers its texture as a new frame whenever the clock has moved.
struct GpuStream {
    texture: wgpu::Texture,
    clock: FakeClock,
    size: FrameSize,
    last: Option<MediaTime>,
    stats: CaptureStats,
    stages: Arc<AtomicU64>,
}

impl CaptureStream for GpuStream {
    fn latest(&mut self) -> Result<Option<VideoSurface>, PlatformError> {
        let now = self.clock.now();
        if self.last == Some(now) {
            return Ok(None);
        }
        self.last = Some(now);
        self.stats.frames_captured += 1;
        let info = VideoFrameInfo {
            size: self.size,
            format: PixelFormat::Rgba8,
            color: ColorInfo {
                range: ColorRange::Full,
                ..ColorInfo::default()
            },
            pts: now,
        };
        let sync = Arc::new(CountSync(Arc::clone(&self.stages)));
        Ok(Some(VideoSurface::Gpu(GpuFrame::new(
            self.texture.clone(),
            info,
            sync,
        ))))
    }

    fn stats(&self) -> CaptureStats {
        self.stats
    }
}

/// A hold with nothing to wait for, counting its stages.
struct CountSync(Arc<AtomicU64>);

impl std::fmt::Debug for CountSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CountSync").finish_non_exhaustive()
    }
}

impl FrameSync for CountSync {
    fn stage(&self, _queue: &wgpu::Queue) -> Result<(), PlatformError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// Fake backends whose GPU interop and capture are `gpu` and a [`DualCapture`] in
/// `mode`.
fn backends(clock: &FakeClock, gpu: Arc<dyn GpuInterop>, mode: GpuMode) -> Backends {
    let size = FrameSize::new(64, 36).expect("size");
    let audio = FakeAudioIo::new(clock.clone(), 48_000, ChannelLayout::Stereo).expect("audio");
    Backends {
        clock: Arc::new(clock.clone()),
        capture: Arc::new(DualCapture {
            cpu: FakeCapture::new(clock.clone(), size),
            clock: clock.clone(),
            size,
            mode,
        }),
        audio: Arc::new(audio),
        codecs: Arc::new(FakeCodecs),
        gpu,
        vcam: None,
        video_devices: Arc::new(FakeVideoDevices::new(clock.clone())),
        media: Arc::new(jstream_platform::fake::FakeMediaDecoders::new()),
    }
}

/// Whether `SoftwareGpu` has a device here. Outside CI a machine with none skips.
fn have_software_gpu() -> bool {
    match SoftwareGpu.open_device() {
        Ok(_) => true,
        Err(err) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI has no wgpu adapter: {err}"
            );
            let _ = writeln!(
                std::io::stderr(),
                "jstream-core: skipping GPU capture test, no adapter: {err}"
            );
            false
        }
    }
}

/// Which path `open_capture` took: a platform context or not, and whether the first
/// frame is on the GPU.
fn path(backends: &Backends) -> (bool, bool) {
    let parts = parts_from(
        backends,
        gpu_draw(),
        GPU_OPEN_TIMEOUT,
        Arc::new(RenderHub::default()),
        Arc::new(Wanted::default()),
        (1280, 720),
    );
    let mut opened = open_capture(&parts).expect("a capture starts on some path");
    let frame = opened
        .stream
        .latest()
        .expect("latest")
        .expect("a first frame");
    (
        opened.context.is_some(),
        matches!(frame, VideoSurface::Gpu(_)),
    )
}

/// Without a platform device the render thread opens its own and capture delivers CPU
/// frames, whatever the capture backend could do. This is the path on every machine
/// without GPU interop, so it needs no adapter.
#[test]
fn without_gpu_interop_capture_stays_on_the_cpu() {
    let clock = FakeClock::new(Timebase::MICROS);
    for mode in [GpuMode::None, GpuMode::Deliver] {
        let backends = backends(&clock, Arc::new(FakeGpuInterop), mode);
        assert_eq!(path(&backends), (false, false), "{mode:?}");
    }
}

/// With a platform device, capture moves onto it when the backend can deliver GPU
/// frames there. A backend with no GPU path, one that refuses the context and one
/// whose GPU capture does not start all fall back to CPU frames, still composited on
/// the platform's device.
#[test]
fn with_gpu_interop_capture_moves_onto_the_device_or_falls_back() {
    let _gpu = one_gpu_test_at_a_time();
    if !have_software_gpu() {
        return;
    }
    let clock = FakeClock::new(Timebase::MICROS);
    for (mode, expected) in [
        (GpuMode::Deliver, (true, true)),
        (GpuMode::None, (true, false)),
        (GpuMode::Refuse, (true, false)),
        (GpuMode::FailStart, (true, false)),
    ] {
        let backends = backends(&clock, Arc::new(SoftwareGpu), mode);
        assert_eq!(path(&backends), expected, "{mode:?}");
    }
}

/// What the render thread's compositor saw: whether it opened on the platform's
/// context, and the first pixel of the newest frame it read back.
#[derive(Default)]
struct Seen {
    on_context: Option<bool>,
    pixel: Option<[u8; 4]>,
}

/// [`gpu_draw`], recording what it was opened on and what it drew into `seen`.
fn watched_draw(seen: Arc<Mutex<Seen>>) -> OpenDraw {
    Arc::new(move |context: Option<&GpuContext>| {
        let mut inner = (gpu_draw())(context)?;
        let seen = Arc::clone(&seen);
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .on_context = Some(context.is_some());
        let draw: DrawFn = Box::new(move |shot, live, size, capture, out, at| {
            let freed = inner(shot, live, size, capture, out, at)?;
            if let Some(pixel) = out.first_chunk::<4>() {
                seen.lock().unwrap_or_else(PoisonError::into_inner).pixel = Some(*pixel);
            }
            Ok(freed)
        });
        Ok(draw)
    })
}

/// An output on the GPU path composites the capture's textures on the platform's
/// device with no CPU copy: the frames are counted as GPU captures, and what is read
/// back for the encoder is the captured colour.
#[test]
fn an_output_composites_gpu_capture_frames() {
    let _gpu = one_gpu_test_at_a_time();
    if !have_software_gpu() {
        return;
    }
    let clock = FakeClock::new(Timebase::MICROS);
    let backends = backends(&clock, Arc::new(SoftwareGpu), GpuMode::Deliver);
    let seen = Arc::new(Mutex::new(Seen::default()));
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends)
        .with_open_timeout(GPU_OPEN_TIMEOUT)
        .with_draw(watched_draw(Arc::clone(&seen)))
        .with_publish(Box::new(RecordingPublish::new()));
    let control = connected(&engine);
    block_on(control.call(output_call("yt", true))).expect("output.start");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut drew_red = false;
    while Instant::now() < deadline {
        clock.advance(50_000);
        thread::sleep(Duration::from_millis(20));
        let stats = engine.output_stats().expect("running");
        let pixel = seen.lock().unwrap_or_else(PoisonError::into_inner).pixel;
        if stats.gpu_captures >= 2 && stats.readbacks >= 2 && pixel == Some(RED) {
            drew_red = true;
            break;
        }
    }
    let stats = engine.output_stats();
    assert!(
        drew_red,
        "no red GPU frame was composited and read back: {stats:?}"
    );
    assert_eq!(
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .on_context,
        Some(true),
        "the compositor opened on the platform's context"
    );
    block_on(control.call(output_call("yt", false))).expect("output.stop");
    assert!(engine.output_stats().is_none());
}
