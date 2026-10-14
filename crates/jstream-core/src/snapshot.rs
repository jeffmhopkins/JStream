//! `video.snapshot`: composite the program scene and encode the readback as a PNG.
//!
//! The compositor lives on its own mutex. Building the shot copies what it needs out of
//! the collection, then readback runs without that lock, so a GPU wait does not stall
//! other sessions. This is a control-plane query, not the render thread (R-RT-002).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use jstream_graph::{Graph, State};
use jstream_media::FrameSize;
use jstream_protocol::{
    Crop as ProtocolCrop, ItemTransform, ProtocolError, QueryResult, Rev, SceneId, SourceId,
    SourceKind,
};
use jstream_render::{
    Color, Compositor, Crop, Image, Item, OpenOptions, Placement, RenderError, Scene, Snapshot,
};

use crate::lock;

/// ADR-0022's default canvas. `canvas.set_size` is not in the protocol yet, so every
/// snapshot uses this size.
const CANVAS_WIDTH: u32 = 1920;
const CANVAS_HEIGHT: u32 = 1080;

/// Longest side of a returned PNG, in pixels (ARCH › MCP).
const MAX_EDGE: u32 = 1280;

/// Straight opacity. The graph does not store it yet, and the compositor's default is 1.
const OPAQUE: f32 = 1.0;

/// Why [`crate::Engine::set_source_frame`] refused a frame.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceFrameError {
    /// Width or height was zero.
    #[error("a source frame needs a non-zero width and height")]
    Empty,
    /// `width * height * 4` does not fit in memory.
    #[error("source frame {width}x{height} is too large")]
    TooLarge {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
    /// The buffer length is not `width * height * 4`.
    #[error("RGBA8 frame is {width}x{height} ({expected} bytes) but {len} bytes were given")]
    Length {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// `width * height * 4`.
        expected: usize,
        /// Bytes provided.
        len: usize,
    },
}

/// The latest straight-alpha sRGB RGBA8 frame of one source.
#[derive(Clone, Debug)]
pub(crate) struct Frame {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
}

impl Frame {
    pub(crate) fn new(width: u32, height: u32, rgba: &[u8]) -> Result<Self, SourceFrameError> {
        if width == 0 || height == 0 {
            return Err(SourceFrameError::Empty);
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(SourceFrameError::TooLarge { width, height })?;
        if rgba.len() != expected {
            return Err(SourceFrameError::Length {
                width,
                height,
                expected,
                len: rgba.len(),
            });
        }
        Ok(Self {
            width,
            height,
            rgba: Arc::from(rgba),
        })
    }
}

/// What one visible item shows.
enum SourceRef {
    /// Bytes from [`Frame`], already on the CPU.
    Frame(Frame),
    /// Another scene, composited first and then placed.
    Scene(SceneId),
}

struct Placed {
    source: SourceRef,
    placement: Placement,
    crop: Crop,
}

/// Everything `draw` needs, owned so the collection lock can be dropped first.
pub(crate) struct Shot {
    rev: Rev,
    /// The first scene in display order. `None` when the collection has no scene.
    program: Option<SceneId>,
    scenes: BTreeMap<SceneId, Vec<Placed>>,
}

/// Copies the program scene and the scenes it nests, at `graph`'s current revision.
pub(crate) fn plan(graph: &Graph, frames: &BTreeMap<SourceId, Frame>) -> Shot {
    let state = graph.state();
    let program = state.scenes().first().map(|scene| scene.id().clone());
    let mut scenes = BTreeMap::new();
    if let Some(id) = &program {
        let mut path = BTreeSet::new();
        collect(state, frames, id, &mut scenes, &mut path);
    }
    Shot {
        rev: graph.rev(),
        program,
        scenes,
    }
}

fn collect(
    state: &State,
    frames: &BTreeMap<SourceId, Frame>,
    id: &SceneId,
    out: &mut BTreeMap<SceneId, Vec<Placed>>,
    path: &mut BTreeSet<SceneId>,
) {
    if out.contains_key(id) || !path.insert(id.clone()) {
        return;
    }
    let mut placed = Vec::new();
    if let Some(scene) = state.scene(id) {
        for item in scene.items() {
            if !item.visible() {
                continue;
            }
            let Some(source) = state.source(item.source_id()) else {
                continue;
            };
            let source = match source.kind() {
                SourceKind::Scene { scene_id } => {
                    collect(state, frames, scene_id, out, path);
                    SourceRef::Scene(scene_id.clone())
                }
                _ => match frames.get(item.source_id()) {
                    Some(frame) => SourceRef::Frame(frame.clone()),
                    None => continue,
                },
            };
            placed.push(Placed {
                source,
                placement: placement(item.transform()),
                crop: crop(item.crop()),
            });
        }
    }
    out.insert(id.clone(), placed);
    path.remove(id);
}

fn placement(transform: &ItemTransform) -> Placement {
    Placement {
        position: [transform.position.x.get(), transform.position.y.get()],
        scale: [transform.scale.x.get(), transform.scale.y.get()],
        rotation_deg: transform.rotation_deg.get(),
    }
}

fn crop(crop: &ProtocolCrop) -> Crop {
    Crop {
        left: crop.left,
        top: crop.top,
        right: crop.right,
        bottom: crop.bottom,
    }
}

/// Renders `shot` and encodes the capped PNG.
pub(crate) fn draw(
    shot: &Shot,
    gpu: &Mutex<Option<Compositor>>,
) -> Result<QueryResult, ProtocolError> {
    let mut guard = compositor(gpu)?;
    let compositor = match guard.as_mut() {
        Some(compositor) => compositor,
        None => {
            return Err(ProtocolError::Internal {
                reason: "the snapshot compositor is not open".into(),
            });
        }
    };
    let mut stack = BTreeSet::new();
    let full = match &shot.program {
        Some(id) => render_scene(compositor, shot, id, &mut stack)?,
        None => paint(compositor, &[])?,
    };
    let (width, height) = fit(full.width(), full.height());
    let rgba = scale_down(full.width(), full.height(), full.rgba(), width, height);
    let png =
        encode_png(width, height, &rgba).map_err(|reason| ProtocolError::Internal { reason })?;
    Ok(QueryResult::VideoSnapshot {
        rev: shot.rev,
        width,
        height,
        png: base64(&png),
    })
}

fn compositor(
    gpu: &Mutex<Option<Compositor>>,
) -> Result<MutexGuard<'_, Option<Compositor>>, ProtocolError> {
    let mut guard = lock(gpu);
    if guard.is_none() {
        match Compositor::open(OpenOptions::headless()) {
            Ok(compositor) => {
                tracing::info!(
                    adapter = %compositor.adapter_description(),
                    "snapshot compositor opened"
                );
                *guard = Some(compositor);
            }
            Err(err) => return Err(from_render(err)),
        }
    }
    Ok(guard)
}

fn render_scene(
    compositor: &mut Compositor,
    shot: &Shot,
    id: &SceneId,
    stack: &mut BTreeSet<SceneId>,
) -> Result<Snapshot, ProtocolError> {
    if !stack.insert(id.clone()) {
        return paint(compositor, &[]);
    }
    let layers = shot.scenes.get(id).map(Vec::as_slice).unwrap_or(&[]);
    let mut stored = Vec::with_capacity(layers.len());
    for layer in layers {
        let image = match &layer.source {
            SourceRef::Frame(frame) => upload(compositor, frame.width, frame.height, &frame.rgba)?,
            SourceRef::Scene(inner) => {
                let nested = render_scene(compositor, shot, inner, stack)?;
                upload(compositor, nested.width(), nested.height(), nested.rgba())?
            }
        };
        stored.push((image, layer.placement, layer.crop));
    }
    let items: Vec<Item<'_>> = stored
        .iter()
        .map(|(image, placement, crop)| Item {
            image,
            placement: *placement,
            crop: *crop,
            opacity: OPAQUE,
            visible: true,
        })
        .collect();
    let snapshot = paint(compositor, &items)?;
    stack.remove(id);
    Ok(snapshot)
}

fn upload(
    compositor: &Compositor,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<Image, ProtocolError> {
    compositor
        .upload_rgba8(width, height, rgba)
        .map_err(from_render)
}

fn paint(compositor: &mut Compositor, items: &[Item<'_>]) -> Result<Snapshot, ProtocolError> {
    let canvas = FrameSize::new(CANVAS_WIDTH, CANVAS_HEIGHT).map_err(|_| RenderError::Invalid {
        what: "canvas",
        detail: "the canvas size is zero".into(),
    });
    let canvas = canvas.map_err(from_render)?;
    let scene = Scene {
        canvas,
        background: Color::BLACK,
        items,
    };
    let output = compositor.render(&scene, true).map_err(from_render)?;
    output
        .pixels()
        .cloned()
        .ok_or_else(|| ProtocolError::Internal {
            reason: "reading the frame back produced no pixels".into(),
        })
}

fn from_render(err: RenderError) -> ProtocolError {
    ProtocolError::Internal {
        reason: err.to_string(),
    }
}

/// The PNG size: the canvas, or the canvas scaled so its longest side is [`MAX_EDGE`].
fn fit(width: u32, height: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= MAX_EDGE || width == 0 || height == 0 {
        return (width, height);
    }
    if width >= height {
        (MAX_EDGE, div_round(height, width).max(1))
    } else {
        (div_round(width, height).max(1), MAX_EDGE)
    }
}

fn div_round(num: u32, den: u32) -> u32 {
    let n = u64::from(num) * u64::from(MAX_EDGE) + u64::from(den) / 2;
    u32::try_from(n / u64::from(den)).unwrap_or(1)
}

/// Nearest-neighbour scale. Equal sizes return `rgba` unchanged, so a 1:1 readback is
/// not resampled.
fn scale_down(src_w: u32, src_h: u32, rgba: &[u8], dst_w: u32, dst_h: u32) -> Vec<u8> {
    if src_w == dst_w && src_h == dst_h {
        return rgba.to_vec();
    }
    let mut out = vec![0u8; dst_w as usize * dst_h as usize * 4];
    for y in 0..dst_h {
        let sy = ((u64::from(y) * u64::from(src_h)) / u64::from(dst_h)) as u32;
        let sy = sy.min(src_h.saturating_sub(1));
        for x in 0..dst_w {
            let sx = ((u64::from(x) * u64::from(src_w)) / u64::from(dst_w)) as u32;
            let sx = sx.min(src_w.saturating_sub(1));
            let from = (sy as usize * src_w as usize + sx as usize) * 4;
            let to = (y as usize * dst_w as usize + x as usize) * 4;
            out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }
    out
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut encoder = png::Encoder::new(&mut buf, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|err| err.to_string())?;
    writer
        .write_image_data(rgba)
        .map_err(|err| err.to_string())?;
    writer.finish().map_err(|err| err.to_string())?;
    Ok(buf)
}

/// Standard base64 with padding and no line breaks.
fn base64(data: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let push = |out: &mut String, n: u32, count: usize| {
        for shift in [18, 12, 6, 0].into_iter().take(count) {
            out.push(ALPHA[((n >> shift) & 63) as usize] as char);
        }
    };
    let mut chunks = data.chunks_exact(3);
    for chunk in chunks.by_ref() {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        push(&mut out, n, 4);
    }
    match chunks.remainder() {
        [a] => {
            push(&mut out, u32::from(*a) << 16, 2);
            out.push('=');
            out.push('=');
        }
        [a, b] => {
            push(&mut out, (u32::from(*a) << 16) | (u32::from(*b) << 8), 3);
            out.push('=');
        }
        _ => {}
    }
    out
}

/// Decodes a PNG to tightly packed RGBA8. Test support for the lavapipe snapshot.
#[cfg(test)]
pub(crate) fn decode_png(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|err| err.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|err| err.to_string())?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(format!(
            "expected RGBA8, got {:?} {:?}",
            info.color_type, info.bit_depth
        ));
    }
    buf.truncate(info.buffer_size());
    Ok((info.width, info.height, buf))
}

/// Standard base64 decode, for tests that check the query's `png` field.
#[cfg(test)]
pub(crate) fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    fn val(byte: u8) -> Result<u8, String> {
        match byte {
            b'A'..=b'Z' => Ok(byte - b'A'),
            b'a'..=b'z' => Ok(byte - b'a' + 26),
            b'0'..=b'9' => Ok(byte - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(format!("not base64: {}", byte as char)),
        }
    }
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err("base64 length is not a multiple of 4".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks_exact(4) {
        let pad = chunk.iter().rev().take_while(|b| **b == b'=').count();
        if pad > 2 {
            return Err("too much base64 padding".into());
        }
        let mut n = 0u32;
        for (i, byte) in chunk.iter().enumerate() {
            let v = if *byte == b'=' {
                0
            } else {
                u32::from(val(*byte)?)
            };
            n |= v << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

/// Whether `error` is the compositor finding no adapter. Tests skip on that outside CI.
#[cfg(test)]
pub(crate) fn is_no_adapter(error: &ProtocolError) -> bool {
    matches!(error, ProtocolError::Internal { reason } if reason.starts_with("no wgpu adapter"))
}

#[cfg(test)]
mod tests {
    use super::{Frame, SourceFrameError, base64, decode_base64, decode_png, encode_png, fit};

    #[test]
    fn canvas_is_capped_on_its_longest_side() {
        assert_eq!(fit(1920, 1080), (1280, 720));
        assert_eq!(fit(1080, 1920), (720, 1280));
        assert_eq!(fit(640, 360), (640, 360));
        assert_eq!(fit(100, 2000), (64, 1280));
    }

    #[test]
    fn base64_round_trips_and_pads() {
        for bytes in [
            b"".as_slice(),
            b"a".as_slice(),
            b"ab".as_slice(),
            b"abc".as_slice(),
            b"abcd".as_slice(),
        ] {
            let text = base64(bytes);
            assert!(!text.contains('\n'), "{text}");
            assert_eq!(decode_base64(&text).expect("decodes"), bytes);
        }
        assert_eq!(base64(&[0xff]), "/w==");
    }

    #[test]
    fn png_round_trips_one_pixel() {
        let png = encode_png(1, 1, &[1, 2, 3, 4]).expect("encodes");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let (w, h, rgba) = decode_png(&png).expect("decodes");
        assert_eq!((w, h, rgba.as_slice()), (1, 1, [1, 2, 3, 4].as_slice()));
    }

    #[test]
    fn a_frame_must_be_non_empty_rgba() {
        assert_eq!(Frame::new(0, 1, &[]).unwrap_err(), SourceFrameError::Empty);
        assert_eq!(
            Frame::new(2, 2, &[0, 0, 0, 0]).unwrap_err(),
            SourceFrameError::Length {
                width: 2,
                height: 2,
                expected: 16,
                len: 4,
            }
        );
        let frame = Frame::new(1, 1, &[9, 8, 7, 6]).expect("one pixel");
        assert_eq!(frame.rgba.as_ref(), &[9, 8, 7, 6]);
    }
}
