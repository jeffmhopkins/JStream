//! `video.snapshot`: composite the program scene and encode the readback as a PNG.
//!
//! Building the shot copies what it needs out of the collection under its lock. The
//! compositor lives on its own thread, `jstream-snapshot`, which opens it, renders,
//! reads back and encodes the PNG. The call's future completes when that thread
//! answers, so the GPU wait and the encode never block the caller's executor thread
//! or other sessions. This is control-plane work, not the render thread (R-RT-002).

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use jstream_graph::{Graph, Placement as GraphPlacement, State};
use jstream_media::FrameSize;
use jstream_protocol::{
    Canvas, Crop as ProtocolCrop, ProtocolError, QueryResult, Rev, SceneId, SourceId, SourceKind,
};
use jstream_render::{
    Color, Compositor, Crop, Image, Item, LegacyTransform, OpenOptions, Placement, RenderError,
    Scene, Snapshot,
};

use crate::lock;

/// Longest side of a returned PNG, in pixels (ARCH › MCP).
const MAX_EDGE: u32 = 1280;

/// Straight opacity. The graph does not store it yet, and the compositor's default is 1.
const OPAQUE: f32 = 1.0;

/// How many levels of scene sources a snapshot draws below the program scene. A scene
/// source deeper than this draws nothing. The graph already rejects cycles; the bound
/// caps what one snapshot can cost, since each level is a canvas-sized FP16 texture.
const MAX_NESTING: usize = 8;

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
#[derive(Clone, Debug, PartialEq, Eq)]
enum SourceRef {
    /// A source's latest [`Frame`], kept in [`Shot::frames`].
    Frame(SourceId),
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
    /// The collection's canvas: the size every scene, nested ones included, is drawn at.
    canvas: Canvas,
    /// The first scene in display order. `None` when the collection has no scene.
    program: Option<SceneId>,
    scenes: BTreeMap<SceneId, Vec<Placed>>,
    /// The frames the copied scenes place, by source.
    frames: BTreeMap<SourceId, Frame>,
}

/// Copies the program scene and the scenes it nests, at `graph`'s current revision.
pub(crate) fn plan(graph: &Graph, frames: &BTreeMap<SourceId, Frame>) -> Shot {
    let state = graph.state();
    let program = state.scenes().first().map(|scene| scene.id().clone());
    let mut shot = Shot {
        rev: graph.rev(),
        canvas: state.canvas(),
        program: program.clone(),
        scenes: BTreeMap::new(),
        frames: BTreeMap::new(),
    };
    if let Some(id) = &program {
        let mut path = BTreeSet::new();
        collect(state, frames, id, &mut shot, &mut path);
    }
    shot
}

fn collect(
    state: &State,
    frames: &BTreeMap<SourceId, Frame>,
    id: &SceneId,
    out: &mut Shot,
    path: &mut BTreeSet<SceneId>,
) {
    if out.scenes.contains_key(id) || !path.insert(id.clone()) {
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
                    Some(frame) => {
                        out.frames
                            .entry(item.source_id().clone())
                            .or_insert_with(|| frame.clone());
                        SourceRef::Frame(item.source_id().clone())
                    }
                    None => continue,
                },
            };
            placed.push(Placed {
                source,
                placement: placement(item.placement()),
                crop: crop(item.crop()),
            });
        }
    }
    out.scenes.insert(id.clone(), placed);
    path.remove(id);
}

/// One composite in a snapshot: a scene at a nesting depth (the program scene is 0).
#[derive(Debug, PartialEq)]
struct Pass {
    scene: SceneId,
    depth: usize,
    layers: Vec<Layer>,
}

/// One item of a [`Pass`].
#[derive(Debug, PartialEq)]
struct Layer {
    image: Source,
    placement: Placement,
    crop: Crop,
}

/// Where a layer's image comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    /// The upload of this source's frame.
    Frame(SourceId),
    /// The image an earlier pass, by index, rendered.
    Pass(usize),
}

/// The composites `draw` makes for the scene `program`, in order: every nested scene
/// before the scenes that place it, the program scene last.
///
/// A scene is rendered once per depth it is reached at, however often it is placed
/// there. A scene source more than [`MAX_NESTING`] levels below the program scene, or
/// one that shows a scene already being drawn further up (a cycle, which the graph
/// rejects), draws nothing.
fn schedule(shot: &Shot, program: &SceneId) -> Vec<Pass> {
    let mut passes = Vec::new();
    let mut done = BTreeMap::new();
    let mut path = BTreeSet::new();
    visit(shot, program, 0, &mut path, &mut done, &mut passes);
    passes
}

fn visit(
    shot: &Shot,
    id: &SceneId,
    depth: usize,
    path: &mut BTreeSet<SceneId>,
    done: &mut BTreeMap<(SceneId, usize), usize>,
    passes: &mut Vec<Pass>,
) -> usize {
    let key = (id.clone(), depth);
    if let Some(index) = done.get(&key) {
        return *index;
    }
    path.insert(id.clone());
    let mut layers = Vec::new();
    for placed in shot.scenes.get(id).map(Vec::as_slice).unwrap_or(&[]) {
        let image = match &placed.source {
            SourceRef::Frame(source) => Source::Frame(source.clone()),
            SourceRef::Scene(inner) => {
                if depth >= MAX_NESTING {
                    tracing::debug!(
                        scene = %inner,
                        max = MAX_NESTING,
                        "nested scene is past the snapshot's depth bound; not drawn"
                    );
                    continue;
                }
                if path.contains(inner) {
                    continue;
                }
                Source::Pass(visit(shot, inner, depth + 1, path, done, passes))
            }
        };
        layers.push(Layer {
            image,
            placement: placed.placement,
            crop: placed.crop,
        });
    }
    path.remove(id);
    passes.push(Pass {
        scene: id.clone(),
        depth,
        layers,
    });
    let index = passes.len() - 1;
    done.insert(key, index);
    index
}

/// For each pass, the index of the last pass that places its image.
fn last_uses(passes: &[Pass]) -> Vec<usize> {
    let mut last: Vec<usize> = (0..passes.len()).collect();
    for (index, pass) in passes.iter().enumerate() {
        for layer in &pass.layers {
            if let Source::Pass(inner) = layer.image {
                last[inner] = last[inner].max(index);
            }
        }
    }
    last
}

/// The compositor's placement for an item's stored one, in the same form (ADR-0022): a
/// legacy transform is evaluated against the frame's size when it is drawn.
fn placement(placement: &GraphPlacement) -> Placement {
    match placement {
        GraphPlacement::Geometry(geometry) => Placement::Geometry(*geometry),
        GraphPlacement::Legacy(transform) => Placement::Legacy(LegacyTransform {
            position: [transform.position.x.get(), transform.position.y.get()],
            scale: [transform.scale.x.get(), transform.scale.y.get()],
            rotation_deg: transform.rotation_deg.get(),
        }),
        // A placement form newer than this engine draws nothing rather than guessing.
        _ => Placement::Legacy(LegacyTransform {
            scale: [0.0, 0.0],
            ..LegacyTransform::IDENTITY
        }),
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

/// How long the snapshot thread waits before it tries an adapter again after an open
/// failed. Each failure in a row doubles it, up to [`MAX_RETRY`]. Until then a snapshot
/// answers with the cached error at once, so a machine without an adapter does not
/// pay for a full adapter probe on every call.
const FIRST_RETRY: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(300);

/// The last failed compositor open, and when the next attempt is allowed.
#[derive(Default)]
pub(crate) struct OpenBackoff {
    failed: Option<(ProtocolError, Instant, Duration)>,
}

impl OpenBackoff {
    /// The cached error while `now` is still inside the wait after a failure.
    pub(crate) fn cached(&self, now: Instant) -> Option<ProtocolError> {
        match &self.failed {
            Some((error, at, wait)) if now.saturating_duration_since(*at) < *wait => {
                Some(error.clone())
            }
            _ => None,
        }
    }

    /// Records a failed open at `now`. The wait doubles from [`FIRST_RETRY`].
    pub(crate) fn failed(&mut self, error: ProtocolError, now: Instant) {
        let wait = match &self.failed {
            Some((_, _, wait)) => (*wait * 2).min(MAX_RETRY),
            None => FIRST_RETRY,
        };
        self.failed = Some((error, now, wait));
    }

    pub(crate) fn opened(&mut self) {
        self.failed = None;
    }
}

/// One snapshot request: the shot, and where to send the answer.
struct Job {
    shot: Shot,
    reply: ReplyTx,
}

/// The `jstream-snapshot` thread, started on the first `video.snapshot`. It owns the
/// compositor. Dropping this closes the queue and joins the thread.
#[derive(Default)]
pub(crate) struct SnapshotWorker {
    queue: Mutex<Option<(Sender<Job>, JoinHandle<()>)>>,
}

impl SnapshotWorker {
    /// Queues `shot`. The returned future resolves when the thread has drawn it.
    pub(crate) fn request(&self, shot: Shot) -> SnapshotReply {
        let (reply, answer) = reply_pair();
        let mut queue = lock(&self.queue);
        if queue.is_none() {
            let (tx, rx) = mpsc::channel();
            match thread::Builder::new()
                .name("jstream-snapshot".into())
                .spawn(move || worker(rx))
            {
                Ok(handle) => *queue = Some((tx, handle)),
                Err(err) => {
                    reply.send(Err(ProtocolError::Internal {
                        reason: format!("starting the snapshot thread: {err}"),
                    }));
                    return answer;
                }
            }
        }
        if let Some((tx, _)) = queue.as_ref()
            && let Err(mpsc::SendError(job)) = tx.send(Job { shot, reply })
        {
            // The thread is gone; dropping the job answers `internal`.
            drop(job);
            *queue = None;
        }
        answer
    }
}

impl Drop for SnapshotWorker {
    fn drop(&mut self) {
        let queue = self
            .queue
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some((tx, handle)) = queue {
            drop(tx);
            if handle.thread().id() != thread::current().id() && handle.join().is_err() {
                tracing::error!("snapshot thread panicked");
            }
        }
    }
}

/// Serves snapshot jobs in order until the queue closes. A panic while drawing is
/// caught per job: that job answers `internal` and the compositor is reopened next time.
fn worker(jobs: Receiver<Job>) {
    let mut gpu: Option<Compositor> = None;
    let mut backoff = OpenBackoff::default();
    for Job { shot, reply } in jobs {
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let compositor = open(&mut gpu, &mut backoff)?;
            draw(&shot, compositor)
        }));
        let result = result.unwrap_or_else(|_| {
            gpu = None;
            Err(ProtocolError::Internal {
                reason: "the snapshot compositor panicked".into(),
            })
        });
        reply.send(result);
    }
}

fn open<'a>(
    gpu: &'a mut Option<Compositor>,
    backoff: &mut OpenBackoff,
) -> Result<&'a mut Compositor, ProtocolError> {
    if gpu.is_none() {
        let now = Instant::now();
        if let Some(error) = backoff.cached(now) {
            return Err(error);
        }
        match Compositor::open(OpenOptions::headless()) {
            Ok(compositor) => {
                tracing::info!(
                    adapter = %compositor.adapter_description(),
                    "snapshot compositor opened"
                );
                backoff.opened();
                *gpu = Some(compositor);
            }
            Err(err) => {
                let error = from_render(err);
                backoff.failed(error.clone(), now);
                return Err(error);
            }
        }
    }
    gpu.as_mut().ok_or_else(|| ProtocolError::Internal {
        reason: "the snapshot compositor is not open".into(),
    })
}

type Answer = Result<QueryResult, ProtocolError>;

#[derive(Default)]
struct Slot {
    answer: Option<Answer>,
    waker: Option<Waker>,
}

/// The sending half of a snapshot answer. Dropped without sending, it answers
/// `internal`, so a caller never waits on a job the thread lost.
struct ReplyTx {
    slot: Option<Arc<Mutex<Slot>>>,
}

impl ReplyTx {
    fn send(mut self, answer: Answer) {
        if let Some(slot) = self.slot.take() {
            fill(&slot, answer);
        }
    }
}

impl Drop for ReplyTx {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            fill(
                &slot,
                Err(ProtocolError::Internal {
                    reason: "the snapshot thread stopped before answering".into(),
                }),
            );
        }
    }
}

fn fill(slot: &Mutex<Slot>, answer: Answer) {
    let waker = {
        let mut slot = lock(slot);
        slot.answer = Some(answer);
        slot.waker.take()
    };
    if let Some(waker) = waker {
        waker.wake();
    }
}

/// The answer to one `video.snapshot`, ready once the snapshot thread has drawn it.
pub(crate) struct SnapshotReply {
    slot: Arc<Mutex<Slot>>,
}

impl Future for SnapshotReply {
    type Output = Answer;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Answer> {
        let mut slot = lock(&self.slot);
        match slot.answer.take() {
            Some(answer) => Poll::Ready(answer),
            None => {
                slot.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

fn reply_pair() -> (ReplyTx, SnapshotReply) {
    let slot = Arc::new(Mutex::new(Slot::default()));
    (
        ReplyTx {
            slot: Some(Arc::clone(&slot)),
        },
        SnapshotReply { slot },
    )
}

/// Renders `shot` and encodes the capped PNG.
fn draw(shot: &Shot, compositor: &mut Compositor) -> Result<QueryResult, ProtocolError> {
    let full = match &shot.program {
        Some(id) => render(compositor, shot, &schedule(shot, id))?,
        None => paint(compositor, shot.canvas, Color::BLACK, &[])?,
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

/// Runs `passes` in order. Each nested scene stays on the GPU as an image (transparent
/// where it draws nothing), dropped after the last pass that places it; the last pass,
/// the program scene, is drawn over black and read back.
fn render(
    compositor: &mut Compositor,
    shot: &Shot,
    passes: &[Pass],
) -> Result<Snapshot, ProtocolError> {
    let last = last_uses(passes);
    let mut uploads: BTreeMap<SourceId, Image> = BTreeMap::new();
    let mut images: Vec<Option<Image>> = Vec::with_capacity(passes.len());
    for (index, pass) in passes.iter().enumerate() {
        for layer in &pass.layers {
            if let Source::Frame(source) = &layer.image
                && !uploads.contains_key(source)
                && let Some(frame) = shot.frames.get(source)
            {
                let image = upload(compositor, frame.width, frame.height, &frame.rgba)?;
                uploads.insert(source.clone(), image);
            }
        }
        let items: Vec<Item<'_>> = pass
            .layers
            .iter()
            .filter_map(|layer| {
                let image = match &layer.image {
                    Source::Frame(source) => uploads.get(source)?,
                    Source::Pass(inner) => images.get(*inner)?.as_ref()?,
                };
                Some(Item {
                    image,
                    placement: layer.placement,
                    crop: layer.crop,
                    opacity: OPAQUE,
                    visible: true,
                })
            })
            .collect();
        if index + 1 == passes.len() {
            return paint(compositor, shot.canvas, Color::BLACK, &items);
        }
        let image = compositor
            .render_image(&scene(shot.canvas, Color::TRANSPARENT, &items)?)
            .map_err(from_render)?;
        drop(items);
        images.push(Some(image));
        for (inner, slot) in images.iter_mut().enumerate() {
            if last[inner] == index {
                *slot = None;
            }
        }
    }
    paint(compositor, shot.canvas, Color::BLACK, &[])
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

/// A scene the size of the collection's canvas.
fn scene<'a>(
    canvas: Canvas,
    background: Color,
    items: &'a [Item<'a>],
) -> Result<Scene<'a>, ProtocolError> {
    let canvas = FrameSize::new(canvas.width, canvas.height).map_err(|_| RenderError::Invalid {
        what: "canvas",
        detail: "the canvas size is zero".into(),
    });
    Ok(Scene {
        canvas: canvas.map_err(from_render)?,
        background,
        items,
    })
}

fn paint(
    compositor: &mut Compositor,
    canvas: Canvas,
    background: Color,
    items: &[Item<'_>],
) -> Result<Snapshot, ProtocolError> {
    let output = compositor
        .render(&scene(canvas, background, items)?, true)
        .map_err(from_render)?;
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
    use super::{
        FIRST_RETRY, Frame, MAX_NESTING, MAX_RETRY, OpenBackoff, Pass, Placed, Shot, Source,
        SourceFrameError, SourceRef, base64, decode_base64, decode_png, draw, encode_png, fit,
        is_no_adapter, last_uses, schedule,
    };
    use jstream_protocol::{Canvas, ProtocolError, QueryResult, Rev, SceneId, SourceId};
    use jstream_render::{Compositor, Crop, LegacyTransform, OpenOptions, Placement};
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::time::{Duration, Instant};

    /// A 16x9 frame, scaled by 120 to cover the 1920x1080 canvas.
    const FULL: Placement = Placement::Legacy(LegacyTransform {
        position: [0.0, 0.0],
        scale: [120.0, 120.0],
        rotation_deg: 0.0,
    });

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];

    fn frame(rgba: [u8; 4]) -> Frame {
        let bytes: Vec<u8> = std::iter::repeat_n(rgba, 16 * 9).flatten().collect();
        Frame::new(16, 9, &bytes).expect("invariant: 16x9 RGBA8")
    }

    fn placed(source: SourceRef, placement: Placement) -> Placed {
        Placed {
            source,
            placement,
            crop: Crop::NONE,
        }
    }

    fn nest(scene: &str, placement: Placement) -> Placed {
        placed(SourceRef::Scene(SceneId::new(scene)), placement)
    }

    fn fill(source: &str) -> Placed {
        placed(SourceRef::Frame(SourceId::new(source)), FULL)
    }

    /// A shot of `scenes`, the first one on program. Sources named `red` and `green`
    /// have frames of that colour.
    fn shot(scenes: Vec<(&str, Vec<Placed>)>) -> Shot {
        let program = scenes.first().map(|(id, _)| SceneId::new(*id));
        Shot {
            rev: Rev(1),
            canvas: Canvas::DEFAULT,
            program,
            scenes: scenes
                .into_iter()
                .map(|(id, items)| (SceneId::new(id), items))
                .collect(),
            frames: BTreeMap::from([
                (SourceId::new("red"), frame(RED)),
                (SourceId::new("green"), frame(GREEN)),
            ]),
        }
    }

    /// A chain `s0` → `s1` → … → `s{last}`, each placing the next at the full canvas,
    /// with green filling the last one over a red program background.
    fn chain(last: usize) -> Shot {
        let mut scenes = Vec::new();
        let names: Vec<String> = (0..=last).map(|i| format!("s{i}")).collect();
        for (i, name) in names.iter().enumerate() {
            let items = if i == 0 {
                vec![fill("red"), nest(&names[1], Placement::IDENTITY)]
            } else if i == last {
                vec![fill("green")]
            } else {
                vec![nest(&names[i + 1], Placement::IDENTITY)]
            };
            scenes.push((name.as_str(), items));
        }
        shot(scenes)
    }

    fn passes(shot: &Shot) -> Vec<(String, usize)> {
        let program = shot.program.as_ref().expect("invariant: a program scene");
        schedule(shot, program)
            .into_iter()
            .map(|pass| (pass.scene.as_str().to_owned(), pass.depth))
            .collect()
    }

    /// Inner scenes come before the scenes that place them, and a scene placed twice at
    /// one depth is composited once.
    #[test]
    fn nested_scenes_are_scheduled_inside_out_once_per_depth() {
        let half = Placement::Legacy(LegacyTransform {
            scale: [0.5, 0.5],
            ..LegacyTransform::IDENTITY
        });
        let shot = shot(vec![
            (
                "program",
                vec![nest("a", half), nest("b", half), nest("a", FULL)],
            ),
            ("a", vec![fill("green"), nest("b", half)]),
            ("b", vec![fill("red")]),
        ]);
        assert_eq!(
            passes(&shot),
            [
                ("b".to_owned(), 2),
                ("a".to_owned(), 1),
                ("b".to_owned(), 1),
                ("program".to_owned(), 0),
            ]
        );
        let program = shot.program.as_ref().expect("invariant: a program scene");
        let schedule: Vec<Pass> = schedule(&shot, program);
        let sources: Vec<&Source> = schedule[3].layers.iter().map(|l| &l.image).collect();
        assert_eq!(
            sources,
            [&Source::Pass(1), &Source::Pass(2), &Source::Pass(1)]
        );
        assert_eq!(last_uses(&schedule), [1, 3, 3, 3]);
    }

    /// The program scene is depth 0. A scene [`MAX_NESTING`] levels down is drawn; one
    /// level further is not.
    #[test]
    fn nesting_stops_at_the_depth_bound() {
        let deepest = passes(&chain(MAX_NESTING));
        assert_eq!(deepest.len(), MAX_NESTING + 1);
        assert_eq!(deepest[0], (format!("s{MAX_NESTING}"), MAX_NESTING));

        let shot = chain(MAX_NESTING + 1);
        let too_deep = passes(&shot);
        assert_eq!(
            too_deep.len(),
            MAX_NESTING + 1,
            "s{} is not drawn",
            MAX_NESTING + 1
        );
        let program = shot.program.as_ref().expect("invariant: a program scene");
        let schedule = schedule(&shot, program);
        assert!(
            schedule[0].layers.is_empty(),
            "the scene at the bound places nothing deeper"
        );
    }

    /// The graph rejects cycles, so a shot never has one; if it did, the scene that
    /// closes it draws nothing instead of recursing.
    #[test]
    fn a_cycle_draws_the_closing_scene_source_as_nothing() {
        let shot = shot(vec![
            ("a", vec![fill("red"), nest("b", Placement::IDENTITY)]),
            ("b", vec![fill("green"), nest("a", Placement::IDENTITY)]),
        ]);
        assert_eq!(passes(&shot), [("b".to_owned(), 1), ("a".to_owned(), 0)]);
        let program = shot.program.as_ref().expect("invariant: a program scene");
        assert_eq!(schedule(&shot, program)[0].layers.len(), 1);
    }

    /// Draws `shot` on a software adapter and decodes the 1280x720 PNG. `None` when there
    /// is no adapter outside CI.
    fn snap(shot: &Shot) -> Option<Vec<u8>> {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let mut compositor = match Compositor::open(OpenOptions::headless()) {
            Ok(compositor) => compositor,
            Err(err) => {
                let error = super::from_render(err);
                assert!(is_no_adapter(&error), "opening the compositor: {error}");
                assert!(
                    std::env::var_os("CI").is_none(),
                    "CI has no wgpu adapter: {error}"
                );
                let _ = writeln!(
                    std::io::stderr(),
                    "jstream-core: skipping nested-scene GPU test, no adapter: {error}"
                );
                return None;
            }
        };
        let result = draw(shot, &mut compositor).expect("draw");
        let QueryResult::VideoSnapshot { png, .. } = result else {
            panic!("not a snapshot: {result:?}");
        };
        let (width, height, rgba) = decode_png(&decode_base64(&png).expect("base64")).expect("png");
        assert_eq!((width, height), (1280, 720));
        Some(rgba)
    }

    /// The pixel at `(x, y)` of a 1280x720 snapshot.
    fn at(rgba: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * 1280 + x as usize) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    /// Two levels: `b` (green) at half size in the bottom-right quarter of `a`, and `a`
    /// at half size at the program's top-left over red. The nested scenes are
    /// transparent where they draw nothing, so red shows around the green.
    #[test]
    fn nested_scenes_composite_through_two_levels() {
        let half = |x: f32, y: f32| {
            Placement::Legacy(LegacyTransform {
                position: [x, y],
                scale: [0.5, 0.5],
                rotation_deg: 0.0,
            })
        };
        let shot = shot(vec![
            ("program", vec![fill("red"), nest("a", half(0.0, 0.0))]),
            ("a", vec![nest("b", half(960.0, 540.0))]),
            ("b", vec![fill("green")]),
        ]);
        let Some(rgba) = snap(&shot) else { return };
        // `b` covers canvas 480..960 x 270..540, which is 320..640 x 180..360 in the PNG.
        for (x, y) in [(330, 190), (480, 270), (630, 350)] {
            assert_eq!(at(&rgba, x, y), GREEN, "({x}, {y})");
        }
        for (x, y) in [(10, 10), (310, 190), (330, 170), (650, 350), (900, 600)] {
            assert_eq!(at(&rgba, x, y), RED, "({x}, {y})");
        }
    }

    /// At the bound the deepest scene still shows; one level past it, nothing does.
    #[test]
    fn the_depth_bound_holds_on_the_gpu() {
        let Some(rgba) = snap(&chain(MAX_NESTING)) else {
            return;
        };
        assert_eq!(at(&rgba, 640, 360), GREEN);
        let Some(rgba) = snap(&chain(MAX_NESTING + 1)) else {
            return;
        };
        assert_eq!(at(&rgba, 640, 360), RED);
    }

    /// A scene placed twice shows in both places, and a cycle still draws.
    #[test]
    fn a_scene_placed_twice_and_a_cycle_both_draw() {
        let left = LegacyTransform {
            scale: [0.5, 1.0],
            ..LegacyTransform::IDENTITY
        };
        let right = Placement::Legacy(LegacyTransform {
            position: [960.0, 0.0],
            ..left
        });
        let left = Placement::Legacy(left);
        let shot = shot(vec![
            ("program", vec![nest("a", left), nest("a", right)]),
            (
                "a",
                vec![fill("green"), nest("program", Placement::IDENTITY)],
            ),
        ]);
        let Some(rgba) = snap(&shot) else { return };
        assert_eq!(at(&rgba, 100, 360), GREEN);
        assert_eq!(at(&rgba, 1100, 360), GREEN);
    }

    /// A failed adapter open is answered from the cache until its wait is over, and
    /// the wait doubles with each failure in a row, up to the cap.
    #[test]
    fn a_failed_open_is_cached_with_backoff() {
        let error = ProtocolError::Internal {
            reason: "no wgpu adapter".into(),
        };
        let start = Instant::now();
        let mut backoff = OpenBackoff::default();
        assert!(backoff.cached(start).is_none(), "nothing failed yet");
        backoff.failed(error.clone(), start);
        assert_eq!(backoff.cached(start), Some(error.clone()));
        assert_eq!(
            backoff.cached(start + FIRST_RETRY - Duration::from_millis(1)),
            Some(error.clone())
        );
        let retry = start + FIRST_RETRY;
        assert!(backoff.cached(retry).is_none(), "the wait is over");
        backoff.failed(error.clone(), retry);
        assert!(backoff.cached(retry + FIRST_RETRY).is_some(), "doubled");
        assert!(backoff.cached(retry + FIRST_RETRY * 2).is_none());
        let mut at = retry;
        for _ in 0..20 {
            at += MAX_RETRY;
            backoff.failed(error.clone(), at);
        }
        assert!(backoff.cached(at + MAX_RETRY).is_none(), "capped");
        backoff.opened();
        assert!(backoff.cached(at).is_none(), "an open clears it");
    }

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
