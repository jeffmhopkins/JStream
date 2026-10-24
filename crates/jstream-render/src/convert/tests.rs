//! The GPU conversion against the CPU reference, within one 8-bit code value per byte, on
//! whatever adapter the compositor tests use (lavapipe, WARP).

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use jstream_media::{
    ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, PixelFormat, Timebase, VideoFrameInfo,
};
use jstream_platform::{FrameSync, GpuFrame, PlatformError};

use super::reference::rgba8_to_nv12;
use super::{Nv12Converter, Nv12Target};
use crate::tests::open;
use crate::{Color, Crop, Item, LegacyTransform, Placement, RenderError, Scene};

const LIMITED: ColorInfo = ColorInfo {
    space: ColorSpace::Bt709,
    range: ColorRange::Limited,
};
const FULL: ColorInfo = ColorInfo {
    space: ColorSpace::Bt709,
    range: ColorRange::Full,
};

fn size(width: u32, height: u32) -> FrameSize {
    FrameSize::new(width, height).expect("invariant: the test size is non-zero")
}

fn source(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("nv12 test source"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        texture.size(),
    );
    texture
}

/// Gradients on every channel, primaries, greys and partial alpha, so every chroma
/// block mixes different colours and the edges are exercised.
fn pattern(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            let px: [u8; 4] = match i % 7 {
                0 => [255, 0, 0, 255],
                1 => [0, 255, 0, 255],
                2 => [0, 0, 255, 255],
                3 => [128, 128, 128, 255],
                4 => [(x * 255 / width) as u8, (y * 255 / height) as u8, 200, 128],
                5 => [255, 255, 255, (i * 37 % 256) as u8],
                _ => [
                    (i * 53 % 256) as u8,
                    (i * 97 % 256) as u8,
                    (i * 11 % 256) as u8,
                    255,
                ],
            };
            out.extend_from_slice(&px);
        }
    }
    out
}

fn convert(
    converter: &Nv12Converter,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    color: ColorInfo,
) -> Vec<u8> {
    let planes = converter
        .planes(size(texture.width(), texture.height()))
        .expect("planes");
    let pass = converter
        .bind(texture, &planes.target(), color)
        .expect("bind");
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    pass.encode(&mut encoder);
    queue.submit([encoder.finish()]);
    planes.read(device, queue).expect("read")
}

fn assert_within_one(actual: &[u8], expected: &[u8], width: usize) {
    assert_eq!(actual.len(), expected.len());
    let luma = expected.len() * 2 / 3;
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        let at = if i < luma {
            format!("Y ({}, {})", i % width, i / width)
        } else {
            let c = i - luma;
            format!(
                "{} ({}, {})",
                ["Cb", "Cr"][c % 2],
                c / 2 % (width / 2),
                c / width
            )
        };
        assert!(a.abs_diff(*e) <= 1, "{at} is {a}, the reference is {e}");
    }
}

#[test]
fn pattern_matches_the_cpu_reference_in_both_ranges() {
    let Some(opened) = open() else {
        return;
    };
    let c = &opened.compositor;
    let (w, h) = (34, 10);
    let rgba = pattern(w, h);
    let texture = source(c.device(), c.queue(), w, h, &rgba);
    let converter = Nv12Converter::new(c.device());
    for (color, range) in [(LIMITED, ColorRange::Limited), (FULL, ColorRange::Full)] {
        let nv12 = convert(&converter, c.device(), c.queue(), &texture, color);
        let expected = rgba8_to_nv12(&rgba, w as usize, h as usize, range);
        assert_within_one(&nv12, &expected, w as usize);
    }
}

#[test]
fn black_white_and_grey_land_on_their_codes() {
    let Some(opened) = open() else {
        return;
    };
    let c = &opened.compositor;
    let converter = Nv12Converter::new(c.device());
    // (pixel, limited Y, full Y). Chroma is neutral for all three.
    for (px, limited, full) in [
        ([0, 0, 0, 255], 16, 0),
        ([255, 255, 255, 255], 235, 255),
        ([128, 128, 128, 255], 126, 128),
        // Transparent white is composited over black.
        ([255, 255, 255, 0], 16, 0),
    ] {
        let rgba: Vec<u8> = std::iter::repeat_n(px, 4 * 2).flatten().collect();
        let texture = source(c.device(), c.queue(), 4, 2, &rgba);
        for (color, y) in [(LIMITED, limited), (FULL, full)] {
            let nv12 = convert(&converter, c.device(), c.queue(), &texture, color);
            assert_eq!(nv12[..8], [y; 8], "{px:?} {color:?}");
            assert_eq!(nv12[8..], [128; 4], "{px:?} {color:?}");
        }
    }
}

#[test]
fn chroma_is_left_sited() {
    let Some(opened) = open() else {
        return;
    };
    let c = &opened.compositor;
    // One red column at x = 3 on grey. Left-sited, it is the right tap of block 1 and
    // the left tap of block 2 (weight 1 of 4 each). Centre-sited chroma would put it
    // half in block 1 and not in block 2 at all.
    let (w, h) = (6, 2);
    let mut rgba = Vec::new();
    for _ in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(if x == 3 {
                &[255, 0, 0, 255]
            } else {
                &[128, 128, 128, 255]
            });
        }
    }
    let texture = source(c.device(), c.queue(), w, h, &rgba);
    let converter = Nv12Converter::new(c.device());
    let nv12 = convert(&converter, c.device(), c.queue(), &texture, LIMITED);
    let cr = |block: usize| i32::from(nv12[12 + block * 2 + 1]) - 128;
    // Pure red's Cr is 112 codes above neutral; the grey's is 0.
    assert_eq!(cr(0), 0);
    assert!((cr(1) - 28).abs() <= 1, "block 1 Cr offset {}", cr(1));
    assert!((cr(2) - 28).abs() <= 1, "block 2 Cr offset {}", cr(2));
    assert_within_one(
        &nv12,
        &rgba8_to_nv12(&rgba, w as usize, h as usize, ColorRange::Limited),
        w as usize,
    );
}

#[test]
fn compositor_output_converts_like_its_readback() {
    let Some(mut opened) = open() else {
        return;
    };
    let (w, h) = (16, 8);
    let image_bytes = pattern(6, 4);
    let image = opened
        .compositor
        .upload_rgba8(6, 4, &image_bytes)
        .expect("upload");
    let items = [Item {
        image: &image,
        placement: Placement::Legacy(LegacyTransform {
            position: [3.0, 2.0],
            ..LegacyTransform::IDENTITY
        }),
        crop: Crop::NONE,
        opacity: 0.75,
        visible: true,
    }];
    let scene = Scene {
        canvas: size(w, h),
        background: Color::rgb(0.2, 0.4, 0.9),
        items: &items,
    };
    let output = opened.compositor.render(&scene, true).expect("render");
    let rgba = output
        .pixels()
        .expect("invariant: readback was requested")
        .rgba()
        .to_vec();
    let c = &opened.compositor;
    let converter = Nv12Converter::new(c.device());
    let nv12 = convert(&converter, c.device(), c.queue(), output.texture(), LIMITED);
    assert_within_one(
        &nv12,
        &rgba8_to_nv12(&rgba, w as usize, h as usize, ColorRange::Limited),
        w as usize,
    );
}

#[test]
fn bad_sizes_formats_and_spaces_are_refused() {
    let Some(opened) = open() else {
        return;
    };
    let c = &opened.compositor;
    let converter = Nv12Converter::new(c.device());
    assert!(matches!(
        converter.planes(size(5, 4)),
        Err(RenderError::Invalid { .. })
    ));
    assert!(matches!(
        converter.planes(size(4, 3)),
        Err(RenderError::Invalid { .. })
    ));
    let texture = source(c.device(), c.queue(), 4, 4, &[0; 64]);
    let planes = converter.planes(size(4, 2)).expect("planes");
    assert!(matches!(
        converter.bind(&texture, &planes.target(), LIMITED),
        Err(RenderError::Invalid { .. })
    ));
    let planes = converter.planes(size(4, 4)).expect("planes");
    let bt601 = ColorInfo {
        space: ColorSpace::Bt601,
        range: ColorRange::Limited,
    };
    assert!(matches!(
        converter.bind(&texture, &planes.target(), bt601),
        Err(RenderError::Invalid { .. })
    ));
    // A plane is not a source.
    assert!(matches!(
        converter.bind(planes.y(), &planes.target(), LIMITED),
        Err(RenderError::UnsupportedFormat { .. })
    ));
    assert!(matches!(
        Nv12Target::nv12_texture(planes.y()),
        Err(RenderError::UnsupportedFormat { .. })
    ));
}

/// The encode tick's step: the frame's sync is staged once, the planes hold the
/// conversion, and the frame's texture is handed back in the same submit. A plane
/// texture stands in for the platform's NV12 encode target, which lavapipe cannot draw.
#[test]
fn submit_stages_the_frame_and_converts_into_it() {
    let Some(opened) = open() else {
        return;
    };
    let c = &opened.compositor;
    let (w, h) = (10, 4);
    let rgba = pattern(w, h);
    let texture = source(c.device(), c.queue(), w, h, &rgba);
    let converter = Nv12Converter::new(c.device());
    let planes = converter.planes(size(w, h)).expect("planes");
    let pass = converter
        .bind(&texture, &planes.target(), FULL)
        .expect("bind");
    let stages = Arc::new(AtomicUsize::new(0));
    let info = VideoFrameInfo {
        size: size(w, h),
        format: PixelFormat::Nv12,
        color: FULL,
        pts: MediaTime::zero(Timebase::MICROS),
    };
    let frame = GpuFrame::new(
        planes.y().clone(),
        info,
        Arc::new(Stages(Arc::clone(&stages))),
    );
    converter.submit(c.queue(), &pass, &frame).expect("submit");
    assert_eq!(stages.load(Ordering::SeqCst), 1);
    assert_within_one(
        &planes.read(c.device(), c.queue()).expect("read"),
        &rgba8_to_nv12(&rgba, w as usize, h as usize, ColorRange::Full),
        w as usize,
    );
}

/// A converter made for a compositor stages its frame under the compositor's submit
/// lock, so a platform thread's submit on the same queue can't come between the
/// staging and the conversion's submit (#667).
#[test]
fn submit_stages_under_the_compositors_submit_lock() {
    let Some(opened) = open() else {
        return;
    };
    let c = &opened.compositor;
    let (w, h) = (2, 2);
    let texture = source(c.device(), c.queue(), w, h, &pattern(w, h));
    let converter = Nv12Converter::for_compositor(c);
    let planes = converter.planes(size(w, h)).expect("planes");
    let pass = converter
        .bind(&texture, &planes.target(), FULL)
        .expect("bind");
    let sync = Arc::new(LockSync {
        lock: c.submit_lock().clone(),
        held: std::sync::Mutex::new(Vec::new()),
    });
    let info = VideoFrameInfo {
        size: size(w, h),
        format: PixelFormat::Nv12,
        color: FULL,
        pts: MediaTime::zero(Timebase::MICROS),
    };
    let frame = GpuFrame::new(
        planes.y().clone(),
        info,
        Arc::clone(&sync) as Arc<dyn FrameSync>,
    );
    converter.submit(c.queue(), &pass, &frame).expect("submit");
    assert_eq!(*sync.held.lock().expect("not poisoned"), [true]);
    assert!(
        c.submit_lock().try_lock().is_some(),
        "the lock is let go after the submit"
    );
}

/// Records, each time it is staged, whether `lock` was held then.
#[derive(Debug)]
struct LockSync {
    lock: jstream_platform::SubmitLock,
    held: std::sync::Mutex<Vec<bool>>,
}

impl FrameSync for LockSync {
    fn stage(&self, _queue: &wgpu::Queue) -> Result<(), PlatformError> {
        let held = self.lock.try_lock().is_none();
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(held);
        Ok(())
    }
}

/// Counts its stages.
#[derive(Debug)]
struct Stages(Arc<AtomicUsize>);

impl FrameSync for Stages {
    fn stage(&self, _queue: &wgpu::Queue) -> Result<(), PlatformError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The native path: an `NV12` texture's plane views as render targets, when the
/// adapter has `TEXTURE_FORMAT_NV12`. Skips (with a line) where it doesn't, and on
/// lavapipe, which has the feature but writes plane 1 into plane 0.
#[test]
fn native_nv12_texture_matches_the_planes_path() {
    let Some(opened) = open() else {
        return;
    };
    let adapter = opened.compositor.adapter();
    if !adapter
        .features()
        .contains(wgpu::Features::TEXTURE_FORMAT_NV12)
    {
        let _ = writeln!(
            std::io::stderr(),
            "jstream-render: adapter has no TEXTURE_FORMAT_NV12, skipping the native test"
        );
        return;
    }
    let info = adapter.get_info();
    if info.backend == wgpu::Backend::Vulkan && info.device_type == wgpu::DeviceType::Cpu {
        // lavapipe offers the feature, but its render to the plane-1 view lands in plane
        // 0's memory: the Y plane's first rows came back holding the CbCr values, and
        // plane 1 stayed zero. ARCH's planes fallback is the path there.
        let _ = writeln!(
            std::io::stderr(),
            "jstream-render: lavapipe renders NV12 plane views wrongly, skipping the native test"
        );
        return;
    }
    let (device, queue) = pollster_ready(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("nv12 native test"),
        required_features: wgpu::Features::TEXTURE_FORMAT_NV12,
        ..Default::default()
    }));
    let (w, h) = (18, 6);
    let rgba = pattern(w, h);
    let texture = source(&device, &queue, w, h, &rgba);
    let nv12 = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("nv12 native target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::NV12,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let converter = Nv12Converter::new(&device);
    let target = Nv12Target::nv12_texture(&nv12).expect("nv12 target");
    let pass = converter.bind(&texture, &target, LIMITED).expect("bind");
    let planes = converter.planes(size(w, h)).expect("planes");
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    pass.encode(&mut encoder);
    for (aspect, plane, (pw, ph)) in [
        (wgpu::TextureAspect::Plane0, planes.y(), (w, h)),
        (wgpu::TextureAspect::Plane1, planes.uv(), (w / 2, h / 2)),
    ] {
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &nv12,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect,
            },
            plane.as_image_copy(),
            wgpu::Extent3d {
                width: pw,
                height: ph,
                depth_or_array_layers: 1,
            },
        );
    }
    queue.submit([encoder.finish()]);
    let out = planes.read(&device, &queue).expect("read");
    assert_within_one(
        &out,
        &rgba8_to_nv12(&rgba, w as usize, h as usize, ColorRange::Limited),
        w as usize,
    );
}

fn pollster_ready<F: std::future::Future<Output = Result<T, wgpu::RequestDeviceError>>, T>(
    future: F,
) -> T {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(result) => result.expect("request_device"),
        std::task::Poll::Pending => panic!("request_device did not finish inline"),
    }
}
