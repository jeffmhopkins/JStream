//! The render thread's GPU capture path and its fallbacks (ARCH › GPU pipeline, step 6),
//! on a software device: which path `open_capture` takes for each kind of platform, an
//! output whose capture frames are textures composited with no CPU copy, a browser
//! source whose page paints on the stage's device, and an output that leaves the GPU
//! path mid-run when compositing keeps failing on it (#451).
//!
//! A software adapter stands in for the platform's media adapter, and a fake capture
//! backend delivers `GpuFrame`s on it. A machine with no adapter skips; CI, which
//! installs one, fails instead.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use jstream_protocol::{Batch, Edit, OutputId, OutputState, Settings, SourceId, SourceKind};
use jstream_render::{Compositor, OpenOptions};
use serde_json::json;

use super::{
    GPU_OPEN_TIMEOUT, add, batch, block_on, connected, create, one_gpu_test_at_a_time, output_call,
    output_state, switch_call,
};
use crate::audio_sources::Wanted;
use crate::runtime::{
    DrawFn, OpenDraw, RenderHub, gpu_draw, open_capture, parts_from, start_capture,
};
use crate::snapshot::CaptureImage;
use crate::{Engine, InProcessControl, RecordingPublish};
use jstream_sources::CaptureSpec;

/// The colour every GPU capture frame composites to.
const RED: [u8; 4] = [255, 0, 0, 255];
/// What every GPU capture frame holds: [`RED`] with alpha below 1, as some drivers and
/// HDR modes deliver. A capture is opaque, so it still composites to [`RED`] (#451).
const CAPTURED: [u8; 4] = [255, 0, 0, 128];

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

/// A [`CAPTURED`] `Rgba8UnormSrgb` texture of `size`, as a capture ring's slot would be.
fn red_texture(device: &wgpu::Device, queue: &wgpu::Queue, size: FrameSize) -> wgpu::Texture {
    filled_texture(
        device,
        queue,
        size,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        CAPTURED,
    )
}

/// A texture of `size` and `format` with every texel `texel`.
fn filled_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    size: FrameSize,
    format: wgpu::TextureFormat,
    texel: [u8; 4],
) -> wgpu::Texture {
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
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let pixels: Vec<u8> = std::iter::repeat_n(texel, (size.width() * size.height()) as usize)
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

/// Which path `open_capture` picked: a platform context or not, and whether the first
/// frame of the primary display's capture is on the GPU.
fn path(backends: &Backends) -> (bool, bool) {
    let parts = parts_from(
        backends,
        gpu_draw(),
        GPU_OPEN_TIMEOUT,
        Arc::new(RenderHub::default()),
        Arc::new(Wanted::default()),
        (1280, 720),
        Arc::default(),
    );
    let opened = open_capture(&parts).expect("a capture path");
    let mut stream = start_capture(&opened.backends, &CaptureSpec::Display(None))
        .expect("a capture starts on some path");
    let frame = stream.latest().expect("latest").expect("a first frame");
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
        let draw: DrawFn = Box::new(move |shot, live, captures, frames, at| {
            let freed = inner(shot, live, captures, frames, at)?;
            // The first mix drawn this tick; with no video mixes, the program.
            let drawn = frames.iter().find(|frame| frame.due);
            if let Some(pixel) = drawn.and_then(|frame| frame.out.first_chunk::<4>()) {
                seen.lock().unwrap_or_else(PoisonError::into_inner).pixel = Some(*pixel);
            }
            Ok(freed)
        });
        Ok(draw)
    })
}

/// An output on the GPU path composites the capture's textures on the platform's
/// device with no CPU copy: the frames are counted as GPU captures, and what is read
/// back for the encoder is the captured colour, opaque whatever alpha the texture holds
/// (#451).
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

/// A browser engine whose pages paint one colour on the device the engine gives it:
/// half-transparent red, premultiplied sRGB code values in BGRA, as CEF paints.
#[derive(Clone, Default)]
struct PaintingBrowser(Arc<Painter>);

/// What [`PaintingBrowser`]'s pages share.
#[derive(Default)]
struct Painter {
    /// The device and queue pages paint on, while the engine gives one.
    device: Mutex<Option<(wgpu::Device, wgpu::Queue)>>,
    /// Each `use_device` call: whether it gave a device.
    given: Mutex<Vec<bool>>,
}

/// Red at alpha 128, premultiplied in code values (128 * 255 / 255), in BGRA order.
const PAGE_TEXEL: [u8; 4] = [0, 0, 128, 128];

impl jstream_browser::BrowserBackend for PaintingBrowser {
    fn open(
        &self,
        spec: &jstream_browser::PageSpec,
    ) -> Result<Box<dyn jstream_browser::BrowserPage>, jstream_browser::BrowserError> {
        Ok(Box::new(PaintedPage {
            painter: Arc::clone(&self.0),
            size: spec.size,
            painted: Mutex::new(None),
            audio: jstream_browser::PageAudio::new(),
        }))
    }

    fn use_device(&self, gpu: Option<&GpuContext>) {
        *self.0.device.lock().unwrap_or_else(PoisonError::into_inner) =
            gpu.map(|gpu| (gpu.device().clone(), gpu.queue().clone()));
        self.0
            .given
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(gpu.is_some());
    }
}

struct PaintedPage {
    painter: Arc<Painter>,
    size: FrameSize,
    /// The frame painted on the current device, kept so a page that hasn't changed
    /// hands out the same texture.
    painted: Mutex<Option<GpuFrame>>,
    audio: Arc<jstream_browser::PageAudio>,
}

impl jstream_browser::BrowserPage for PaintedPage {
    fn frame(&self) -> Option<GpuFrame> {
        let device = self
            .painter
            .device
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (device, queue) = device.as_ref()?;
        let mut painted = self.painted.lock().unwrap_or_else(PoisonError::into_inner);
        if painted.is_none() {
            let texture = filled_texture(
                device,
                queue,
                self.size,
                wgpu::TextureFormat::Bgra8Unorm,
                PAGE_TEXEL,
            );
            let info = VideoFrameInfo {
                size: self.size,
                format: PixelFormat::Bgra8,
                color: ColorInfo {
                    range: ColorRange::Full,
                    ..ColorInfo::default()
                },
                pts: MediaTime::zero(Timebase::MICROS),
            };
            let sync = Arc::new(CountSync(Arc::new(AtomicU64::new(0))));
            *painted = Some(GpuFrame::new(texture, info, sync));
        }
        painted.clone()
    }

    fn set_css(&mut self, _css: &str) {}

    fn audio(&self) -> &Arc<jstream_browser::PageAudio> {
        &self.audio
    }

    fn denied_requests(&self) -> u64 {
        0
    }
}

/// On the GPU path a browser page paints on the program stage's device, and the render
/// thread samples its texture there as premultiplied sRGB: half-transparent red over
/// the black canvas reads back as linear red one half, not the darker red reading it
/// as straight alpha would give. The stage takes the device away when it stops.
#[test]
fn a_browser_page_is_composited_from_its_gpu_frames() {
    let _gpu = one_gpu_test_at_a_time();
    if !have_software_gpu() {
        return;
    }
    let clock = FakeClock::new(Timebase::MICROS);
    let backends = backends(&clock, Arc::new(SoftwareGpu), GpuMode::None);
    let browser = PaintingBrowser::default();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let engine = Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends)
        .with_browser(Arc::new(browser.clone()))
        .with_open_timeout(GPU_OPEN_TIMEOUT)
        .with_draw(watched_draw(Arc::clone(&seen)))
        .with_publish(Box::new(RecordingPublish::new()));
    let control = connected(&engine);
    show_a_page(&control);
    block_on(control.call(output_call("yt", true))).expect("output.start");

    // Linear red 128/255, premultiplied over black, encoded back to sRGB.
    let half = 128.0_f32 / 255.0;
    let red = (1.055 * half.powf(1.0 / 2.4) - 0.055) * 255.0;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut pixel = None;
    while Instant::now() < deadline {
        clock.advance(50_000);
        thread::sleep(Duration::from_millis(20));
        pixel = seen.lock().unwrap_or_else(PoisonError::into_inner).pixel;
        if pixel.is_some_and(|[r, g, b, a]| {
            (f32::from(r) - red).abs() <= 3.0 && g == 0 && b == 0 && a == 255
        }) {
            break;
        }
    }
    let got = pixel.map(|[r, g, b, a]| (f32::from(r), g, b, a));
    assert!(
        got.is_some_and(|(r, g, b, a)| (r - red).abs() <= 3.0 && g == 0 && b == 0 && a == 255),
        "the page was not composited as premultiplied red: {pixel:?}, expected red {red:.0}"
    );
    assert_eq!(
        *browser
            .0
            .given
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
        [true],
        "the browser got the stage's device"
    );

    block_on(control.call(output_call("yt", false))).expect("output.stop");
    assert!(engine.output_stats().is_none());
    assert_eq!(
        *browser
            .0
            .given
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
        [true, false],
        "the stage took the device away when it stopped"
    );
}

/// Puts a scene showing a `browser` source on program.
fn show_a_page(control: &InProcessControl) {
    batch(
        control,
        Batch {
            edits: vec![
                create("main"),
                Edit::SourceCreate {
                    source_id: SourceId::new("page"),
                    name: "Page".into(),
                    kind: SourceKind::Builtin {
                        kind: "browser".into(),
                    },
                    settings: Settings(
                        [("url".to_owned(), json!("https://example.com/"))]
                            .into_iter()
                            .collect(),
                    ),
                    duplicated_from: None,
                },
                add("main", "item", "page"),
            ],
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        },
    )
    .expect("a scene showing the page");
    block_on(control.call(switch_call("main"))).expect("scene.switch");
}

/// What a [`breaking_draw`] fails, standing in for the two ways the GPU path breaks
/// mid-run (#451).
#[derive(Clone, Copy, Debug)]
enum Breaks {
    /// Every frame that shows a GPU capture frame fails, as an import of another
    /// backend's texture would.
    GpuFrames,
    /// Every frame fails while on the platform's context, as on a removed device.
    Context,
    /// As `Context`, and the compositor cannot open on a device of its own either.
    Everywhere,
}

/// What a [`breaking_draw`] saw: the device each open was on (`true` for the
/// platform's context), and the first pixel of the newest frame drawn without one.
#[derive(Default)]
struct Opens {
    on_context: Vec<bool>,
    own_pixel: Option<[u8; 4]>,
}

/// What a `FakeCapture` CPU frame composites to: all-zero BGRA, alpha ignored.
const BLACK: [u8; 4] = [0, 0, 0, 255];

/// [`gpu_draw`], failing as `breaks` says.
fn breaking_draw(breaks: Breaks, opens: Arc<Mutex<Opens>>) -> OpenDraw {
    Arc::new(move |context: Option<&GpuContext>| {
        let on_context = context.is_some();
        opens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .on_context
            .push(on_context);
        if !on_context && matches!(breaks, Breaks::Everywhere) {
            return Err("no device of its own in this test".to_owned());
        }
        let mut inner = (gpu_draw())(context)?;
        let opens = Arc::clone(&opens);
        let draw: DrawFn = Box::new(move |shot, live, captures, frames, at| {
            let gpu_frame = captures
                .frame(&CaptureSpec::Display(None))
                .is_some_and(|frame| matches!(frame.image, CaptureImage::Gpu(_)));
            match breaks {
                Breaks::GpuFrames if gpu_frame => return Err("unsupported format".to_owned()),
                Breaks::Context | Breaks::Everywhere if on_context => {
                    return Err("device removed".to_owned());
                }
                _ => {}
            }
            let freed = inner(shot, live, captures, frames, at)?;
            let drawn = frames.iter().find(|frame| frame.due);
            if !on_context && let Some(pixel) = drawn.and_then(|frame| frame.out.first_chunk::<4>())
            {
                opens
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .own_pixel = Some(*pixel);
            }
            Ok(freed)
        });
        Ok(draw)
    })
}

/// An engine on the software GPU path, capturing per `mode`, whose compositor breaks as
/// `breaks` says.
fn breaking_engine(
    clock: &FakeClock,
    mode: GpuMode,
    breaks: Breaks,
    opens: &Arc<Mutex<Opens>>,
) -> Engine {
    Engine::new()
        .with_output(OutputId::new("yt"))
        .with_backends(backends(clock, Arc::new(SoftwareGpu), mode))
        .with_open_timeout(GPU_OPEN_TIMEOUT)
        .with_draw(breaking_draw(breaks, Arc::clone(opens)))
        .with_publish(Box::new(RecordingPublish::new()))
}

/// Advances the clock a tick at a time until `done` holds or 30 s pass; whether it held.
fn tick_until(clock: &FakeClock, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        clock.advance(50_000);
        thread::sleep(Duration::from_millis(10));
        if done() {
            return true;
        }
    }
    false
}

/// When compositing keeps failing on the GPU path mid-run, because every GPU capture
/// frame fails to import or because the context's device went away, the output does
/// not fail: the render thread reopens the compositor on a device of its own, restarts
/// the capture on the CPU path and carries on, and the fallback is counted (#451).
#[test]
fn an_output_falls_back_from_the_gpu_path_mid_run() {
    let _gpu = one_gpu_test_at_a_time();
    if !have_software_gpu() {
        return;
    }
    for breaks in [Breaks::GpuFrames, Breaks::Context] {
        let clock = FakeClock::new(Timebase::MICROS);
        let opens = Arc::new(Mutex::new(Opens::default()));
        let engine = breaking_engine(&clock, GpuMode::Deliver, breaks, &opens);
        let control = connected(&engine);
        block_on(control.call(output_call("yt", true))).expect("output.start");
        let mut stats = None;
        let fell_back = tick_until(&clock, || {
            let now = engine.output_stats().expect("still running");
            let pixel = opens
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .own_pixel;
            let done = now.gpu_fallbacks == 1 && pixel == Some(BLACK);
            stats = Some(now);
            done
        });
        assert!(
            fell_back,
            "{breaks:?}: no fallback to the CPU path: {stats:?}"
        );
        let stats = stats.expect("read");
        // The CPU path keeps drawing: more frames are read back, none on the GPU path.
        let mut later = None;
        let drew = tick_until(&clock, || {
            let now = engine.output_stats().expect("still running");
            let done = now.readbacks >= stats.readbacks + 3;
            later = Some(now);
            done
        });
        let later = later.expect("read");
        assert!(drew, "{breaks:?}: {later:?}");
        assert_eq!(
            later.gpu_captures, stats.gpu_captures,
            "{breaks:?}: off the GPU path"
        );
        assert_eq!(later.gpu_fallbacks, 1, "{breaks:?}");
        assert_eq!(
            output_state(&control, "yt"),
            OutputState::Active,
            "{breaks:?}"
        );
        assert_eq!(
            opens
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .on_context,
            vec![true, false],
            "{breaks:?}: opened on the context, then on its own device"
        );
        block_on(control.call(output_call("yt", false))).expect("output.stop");
        assert!(engine.output_stats().is_none());
    }
}

/// A fallback from the GPU path takes the context away from the browser pages painting
/// on it, while the output runs on, since the compositor can no longer sample them
/// there (#451).
#[test]
fn the_gpu_fallback_takes_the_context_from_browser_pages() {
    let _gpu = one_gpu_test_at_a_time();
    if !have_software_gpu() {
        return;
    }
    let clock = FakeClock::new(Timebase::MICROS);
    let opens = Arc::new(Mutex::new(Opens::default()));
    let browser = PaintingBrowser::default();
    let engine = breaking_engine(&clock, GpuMode::None, Breaks::Context, &opens)
        .with_browser(Arc::new(browser.clone()));
    let control = connected(&engine);
    show_a_page(&control);
    block_on(control.call(output_call("yt", true))).expect("output.start");
    let given = || {
        browser
            .0
            .given
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    };
    let released = tick_until(&clock, || given() == [true, false]);
    assert!(released, "the browser still has the device: {:?}", given());
    let stats = engine.output_stats().expect("still running");
    assert_eq!(stats.gpu_fallbacks, 1, "{stats:?}");
    assert_eq!(output_state(&control, "yt"), OutputState::Active);
    block_on(control.call(output_call("yt", false))).expect("output.stop");
    assert_eq!(given(), [true, false], "taken away once");
}

/// A fallback that cannot open the compositor on a device of its own fails the output,
/// instead of riding out more errors on the broken path.
#[test]
fn an_output_fails_when_the_gpu_fallback_fails() {
    let _gpu = one_gpu_test_at_a_time();
    if !have_software_gpu() {
        return;
    }
    let clock = FakeClock::new(Timebase::MICROS);
    let opens = Arc::new(Mutex::new(Opens::default()));
    let engine = breaking_engine(&clock, GpuMode::Deliver, Breaks::Everywhere, &opens);
    let control = connected(&engine);
    block_on(control.call(output_call("yt", true))).expect("output.start");
    let failed = tick_until(&clock, || {
        output_state(&control, "yt") == OutputState::Failed && engine.output_stats().is_none()
    });
    assert!(
        failed,
        "the output did not fail: {:?}",
        engine.output_stats()
    );
    assert_eq!(
        opens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .on_context,
        vec![true, false],
        "one fallback was tried"
    );
}

/// How a fallback's own open misbehaves in [`stuck_fallback_draw`].
#[derive(Clone, Copy, Debug)]
enum OwnOpen {
    /// Blocks until the test lets it go, as an adapter that hangs while opening.
    Hangs,
    /// Panics.
    Panics,
}

/// A compositor whose every frame fails on the platform's context, so the render thread
/// falls back, and whose open on a device of its own misbehaves as `own` says. It draws
/// nothing itself, so the first open is immediate and a short deadline can be tested.
fn stuck_fallback_draw(own: OwnOpen, release: Arc<AtomicBool>) -> OpenDraw {
    Arc::new(move |context: Option<&GpuContext>| {
        if context.is_some() {
            let draw: DrawFn = Box::new(|_, _, _, _, _| Err("device removed".to_owned()));
            return Ok(draw);
        }
        match own {
            OwnOpen::Hangs => {
                while !release.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(5));
                }
                Err("let go by the test".to_owned())
            }
            OwnOpen::Panics => panic!("the adapter fell over"),
        }
    })
}

/// A fallback whose compositor hangs while opening, or panics, fails the output within
/// the open deadline, instead of freezing the program on the render thread (#577).
#[test]
fn an_output_fails_when_the_gpu_fallback_hangs_or_panics() {
    let _gpu = one_gpu_test_at_a_time();
    if !have_software_gpu() {
        return;
    }
    for own in [OwnOpen::Hangs, OwnOpen::Panics] {
        let clock = FakeClock::new(Timebase::MICROS);
        let release = Arc::new(AtomicBool::new(false));
        let engine = Engine::new()
            .with_output(OutputId::new("yt"))
            .with_backends(backends(&clock, Arc::new(SoftwareGpu), GpuMode::None))
            .with_open_timeout(Duration::from_secs(2))
            .with_draw(stuck_fallback_draw(own, Arc::clone(&release)))
            .with_publish(Box::new(RecordingPublish::new()));
        let control = connected(&engine);
        // Lets a hanging open finish when the test ends, passed or failed. Declared after
        // the engine, so it runs first: an engine whose render thread is stuck in the open
        // could not drop before it.
        struct Release(Arc<AtomicBool>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let _release = Release(Arc::clone(&release));
        block_on(control.call(output_call("yt", true))).expect("output.start");
        let started = Instant::now();
        let failed = tick_until(&clock, || {
            output_state(&control, "yt") == OutputState::Failed && engine.output_stats().is_none()
        });
        assert!(failed, "{own:?}: the output did not fail");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{own:?}: failed only after {:?}",
            started.elapsed()
        );
    }
}
