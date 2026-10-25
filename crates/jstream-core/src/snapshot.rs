//! The program mix as the render thread and `video.snapshot` draw it, and
//! `video.snapshot` itself.
//!
//! [`plan`] copies what the program scene needs out of the collection into an immutable
//! [`Shot`], under the collection lock, and schedules its passes; the engine publishes it
//! as the render snapshot ([`crate::runtime::RenderHub`]). [`compose`] draws a shot with
//! a [`ResourceCache`], so a frame that shows what the last one showed allocates no
//! texture: the render thread calls it every tick, the snapshot thread for every
//! `video.snapshot`.
//!
//! The snapshot compositor lives on its own thread, `jstream-snapshot`, which opens it,
//! renders, reads back and encodes the PNG. The call's future completes when that thread
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

use jstream_graph::{
    Filter, Graph, Item as GraphItem, Placement as GraphPlacement, Scene as GraphScene, Source,
    State,
};
use jstream_media::{FrameSize, MediaTime, PixelFormat, Rounding, VideoFrame};
use jstream_platform::{ContextKey, GpuFrame};
use jstream_protocol::{
    Canvas, Crop as ProtocolCrop, Edit, FilterChain, Glide, ItemGeometry, ItemId, ItemTransition,
    ProtocolError, QueryResult, Rev, SceneId, SourceId, SourceKind, VideoMixId,
};
use jstream_render::transition::{Endpoint, Track};
use jstream_render::{
    Color, Compositor, Crop, Item, LegacyTransform, OpenOptions, Output, Placement, RenderError,
    ResourceCache, Scene,
};
use jstream_sources::CaptureSpec;

use crate::chain::{Chain, ChainOwner, Chains};
use crate::filter_assets::FilterAssets;
use crate::lock;
use crate::runtime::RenderSnapshot;
use crate::transition::{self, From, Planned, Running, Visual};

/// Longest side of a returned PNG, in pixels (ARCH › MCP).
const MAX_EDGE: u32 = 1280;

/// Straight opacity 1: how the program layer is scaled onto a differently sized output.
const OPAQUE: f32 = 1.0;

/// How many levels of scene sources the program mix draws below the program scene, on
/// the render thread and in `video.snapshot` alike. A scene source deeper than this
/// draws nothing. The graph already rejects cycles; the bound caps what one frame can
/// cost, since each level is a canvas-sized FP16 texture. ARCH › Data model states it
/// for clients.
pub(crate) const MAX_NESTING: usize = 8;

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

/// The latest frame of one source: straight-alpha sRGB RGBA8 that `set_source_frame`
/// stored or its live source produced, or a live source's texture on the program stage's
/// GPU device (a browser page).
#[derive(Clone, Debug)]
pub(crate) struct Frame {
    width: u32,
    height: u32,
    /// The size it is placed at: its own, unless its source draws it at a density
    /// (ADR-0073), when it has more pixels than that.
    extent: [u32; 2],
    pixels: Pixels,
    /// Which `set_source_frame` stored it or which new frame a live source produced,
    /// counted per engine. Two frames with the same version are the same bytes, so a
    /// shot compares and a cache re-uploads by it.
    version: u64,
}

/// A frame's tightly packed bytes, shared with whoever made them.
#[derive(Clone, Debug)]
enum Pixels {
    /// Copied from `set_source_frame`.
    Copied(Arc<[u8]>),
    /// A live source's frame, packed `Rgba8` in its first plane.
    Video(Arc<VideoFrame>),
    /// A live source's texture, premultiplied sRGB code values, on the GPU context
    /// `context`. Only a compositor on that context imports it ([`compose`]); others draw
    /// nothing for it.
    Gpu {
        frame: GpuFrame,
        context: ContextKey,
    },
}

impl PartialEq for Frame {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.width == other.width
            && self.height == other.height
            && self.extent == other.extent
    }
}

impl Frame {
    pub(crate) fn new(
        width: u32,
        height: u32,
        rgba: &[u8],
        version: u64,
    ) -> Result<Self, SourceFrameError> {
        let expected = rgba_len(width, height)?;
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
            extent: [width, height],
            pixels: Pixels::Copied(Arc::from(rgba)),
            version,
        })
    }

    /// A live source's frame, shared rather than copied when it is packed `Rgba8` (as
    /// every built-in kind's is). Another layout is repacked; a frame that isn't RGBA8
    /// or is empty gives `None`, and the source draws nothing.
    pub(crate) fn from_video(video: Arc<VideoFrame>, version: u64) -> Option<Self> {
        let info = *video.info();
        if info.format != PixelFormat::Rgba8 {
            return None;
        }
        let (width, height) = (info.size.width(), info.size.height());
        let expected = rgba_len(width, height).ok()?;
        let plane = video.plane(0)?;
        let row = width as usize * 4;
        let pixels = if plane.stride() == row && plane.data().len() >= expected {
            Pixels::Video(video)
        } else {
            let mut packed = Vec::with_capacity(expected);
            for y in 0..height {
                packed.extend_from_slice(video.row(0, y)?.get(..row)?);
            }
            Pixels::Copied(Arc::from(packed))
        };
        Some(Self {
            width,
            height,
            extent: [width, height],
            pixels,
            version,
        })
    }

    /// The same frame placed as if it were `extent`: a frame its source drew at a
    /// density ([`jstream_sources::Source::set_density`]).
    pub(crate) fn placed_at(self, extent: FrameSize) -> Self {
        Self {
            extent: [extent.width(), extent.height()],
            ..self
        }
    }

    /// A live source's texture on `context`, the program stage's GPU context: sRGB code
    /// values already multiplied by alpha, as a browser page paints (ADR-0021).
    pub(crate) fn from_gpu(frame: GpuFrame, context: ContextKey, version: u64) -> Self {
        let size = frame.info().size;
        Self {
            width: size.width(),
            height: size.height(),
            extent: [size.width(), size.height()],
            pixels: Pixels::Gpu { frame, context },
            version,
        }
    }

    /// The same bytes, as the frame `set_source_frame` stored under `version`.
    pub(crate) fn with_version(self, version: u64) -> Self {
        Self { version, ..self }
    }

    /// The tightly packed RGBA8 bytes, `width * height * 4` of them. Empty for a GPU
    /// frame, which has none on the CPU.
    fn rgba(&self) -> &[u8] {
        let len = self.width as usize * self.height as usize * 4;
        match &self.pixels {
            Pixels::Copied(bytes) => bytes,
            Pixels::Video(video) => video
                .plane(0)
                .and_then(|plane| plane.data().get(..len))
                .unwrap_or_default(),
            Pixels::Gpu { .. } => &[],
        }
    }

    /// Its size in pixels and the size it is placed at, for tests.
    #[cfg(test)]
    pub(crate) fn sizes(&self) -> ([u32; 2], [u32; 2]) {
        ([self.width, self.height], self.extent)
    }

    /// The first pixel, for tests that check what a source shows.
    #[cfg(test)]
    pub(crate) fn first_pixel(&self) -> Option<[u8; 4]> {
        self.rgba().get(..4)?.try_into().ok()
    }
}

/// `width * height * 4`, for a frame with no zero side.
fn rgba_len(width: u32, height: u32) -> Result<usize, SourceFrameError> {
    if width == 0 || height == 0 {
        return Err(SourceFrameError::Empty);
    }
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(SourceFrameError::TooLarge { width, height })
}

/// What one visible item shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceRef {
    /// A source's latest [`Frame`], kept in [`Shot::frames`].
    Frame(SourceId),
    /// Another scene, composited first and then placed.
    Scene(SceneId),
    /// A `display` or `window` source with no stored frame: what the output pipeline
    /// captures for it, on the render thread. Each source has a capture of its own, so a
    /// backend that keeps a grant per source (the ScreenCast portal's restore token)
    /// keeps each source's apart. `video.snapshot` has no capture, so it draws nothing
    /// there.
    Capture(CaptureKey),
    /// A source's frame or capture after its video chain, kept in [`Shot::chains`].
    Filtered(SourceId),
    /// An item's box after its effects (ADR-0065), as that scene draws the item: what a
    /// transition's track shows for an item with effects.
    Boxed(SceneId, ItemId),
}

#[derive(Debug, PartialEq)]
struct Placed {
    /// The item, whose live values (ADR-0032) override `placement` and `crop`.
    item: ItemId,
    source: SourceRef,
    placement: Placement,
    crop: Crop,
    /// The item's straight alpha multiplier (ADR-0023), in [0, 1].
    opacity: f32,
    /// Its effects' box, when an effect of it runs: the layer then shows the effects'
    /// output in place of `source`.
    effects: Option<Boxed>,
}

/// The longest side of an item's effects layer, in pixels: a larger box plus outset is
/// drawn into it scaled down uniformly (ADR-0065 §5).
const MAX_LAYER_SIDE: u32 = 8192;

/// The layer an item with effects is drawn into (ADR-0065 §5): its box as planned plus
/// the effects' outset on every side, in canvas pixels, scaled down to fit
/// [`MAX_LAYER_SIDE`] when larger. A live resize draws into the same layer, scaled, until
/// the plan is made again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Boxed {
    /// The box as planned, in canvas pixels.
    size: [f32; 2],
    /// The room the effects need beyond each edge, in canvas pixels.
    outset: f32,
    /// The layer's size in pixels.
    layer: [u32; 2],
    /// Layer pixels per canvas pixel: 1, or less for a layer scaled down to fit.
    scale: f32,
    /// The size of the image the item shows, when known: what turns a legacy transform
    /// into its box.
    input: Option<[u32; 2]>,
}

impl Boxed {
    /// The layer for a box placed at `geometry` with effects that need `outset` pixels,
    /// over an image of `input`. `None` for a box that isn't finite.
    fn new(geometry: &ItemGeometry, outset: u32, input: Option<[u32; 2]>) -> Option<Self> {
        let size = [geometry.size.width.get(), geometry.size.height.get()];
        let outset = outset as f32;
        let padded = [size[0] + 2.0 * outset, size[1] + 2.0 * outset];
        if !padded.iter().all(|side| side.is_finite() && *side > 0.0) {
            return None;
        }
        let scale = (MAX_LAYER_SIDE as f32 / padded[0].max(padded[1])).min(1.0);
        let side = |px: f32| ((px * scale).ceil() as u32).clamp(1, MAX_LAYER_SIDE);
        Some(Self {
            size,
            outset,
            layer: [side(padded[0]), side(padded[1])],
            scale,
            input,
        })
    }

    /// The box `placement` puts an item cropped by `crop` in: its geometry, or a legacy
    /// transform's equivalent over the image's size. `None` when that isn't known or the
    /// placement draws nothing.
    fn geometry_of(&self, placement: Placement, crop: Crop) -> Option<ItemGeometry> {
        match placement {
            Placement::Geometry(geometry) => Some(geometry),
            Placement::Legacy(transform) => {
                let [width, height] = self.input?;
                let transform = jstream_protocol::ItemTransform {
                    position: jstream_protocol::Vec2::new(
                        transform.position[0],
                        transform.position[1],
                    )?,
                    scale: jstream_protocol::Vec2::new(transform.scale[0], transform.scale[1])?,
                    rotation_deg: jstream_protocol::Finite32::new(transform.rotation_deg)?,
                };
                let cut = ProtocolCrop {
                    left: crop.left,
                    top: crop.top,
                    right: crop.right,
                    bottom: crop.bottom,
                };
                ItemGeometry::from_legacy(transform, cut, width, height)
                    .ok()
                    .flatten()
                    .map(|boxed| boxed.geometry)
            }
        }
    }

    /// Where the item is drawn inside its layer: centred at the box's planned size,
    /// flat and unrotated, with `geometry`'s fit and flips, so the effects see the
    /// cropped, fitted box in its own frame (ADR-0050 §7).
    fn inside(&self, geometry: &ItemGeometry) -> Option<ItemGeometry> {
        let centre =
            jstream_protocol::Vec2::new(self.layer[0] as f32 * 0.5, self.layer[1] as f32 * 0.5)?;
        Some(ItemGeometry {
            center: centre,
            size: jstream_protocol::Size::try_new(
                self.size[0] * self.scale,
                self.size[1] * self.scale,
            )
            .ok()?,
            fit_mode: geometry.fit_mode,
            flip_h: geometry.flip_h,
            flip_v: geometry.flip_v,
            ..ItemGeometry::covering_canvas(1, 1).ok()?
        })
    }

    /// Where the layer is placed for an item at `geometry`: its box grown by the outset
    /// on every side, in proportion to the box the layer was planned for, the whole
    /// layer stretched over it, turned and projected as the box is.
    fn around(&self, geometry: &ItemGeometry) -> Option<ItemGeometry> {
        let grow = |live: f32, planned: f32| live * (planned + 2.0 * self.outset) / planned;
        Some(ItemGeometry {
            size: jstream_protocol::Size::try_new(
                grow(geometry.size.width.get(), self.size[0]),
                grow(geometry.size.height.get(), self.size[1]),
            )
            .ok()?,
            fit_mode: jstream_protocol::FitMode::Stretch,
            flip_h: false,
            flip_v: false,
            ..*geometry
        })
    }
}

/// An item with effects as a shot draws it (ADR-0065 §5): what the item shows, where
/// it is placed as planned, and its layer.
#[derive(Clone, Debug, PartialEq)]
struct ItemBox {
    item: ItemId,
    source: SourceRef,
    placement: Placement,
    crop: Crop,
    boxed: Boxed,
}

/// The program mix and every video mix, copied out of the collection so they can be
/// drawn without the collection lock: what each reachable scene places, the frames they
/// show, and the passes that draw them. Immutable once [`plan`] returns it; the engine
/// publishes it to the render thread through `arc-swap`. The default is an empty
/// collection's.
#[derive(Debug)]
pub(crate) struct Shot {
    /// The collection's canvas: the size every scene, nested ones included, is drawn at.
    canvas: Canvas,
    /// The scene on program (ADR-0030). `None` when the collection has no scene.
    program: Option<SceneId>,
    scenes: BTreeMap<SceneId, Vec<Placed>>,
    /// The frames the copied scenes place, by source.
    frames: BTreeMap<SourceId, Frame>,
    /// The video chains of the sources they place that have one (ADR-0026), and of the
    /// scenes they draw that have one (ADR-0065).
    chains: BTreeMap<ChainOwner, Chain>,
    /// [`schedule`] of the program scene, made once here so drawing a frame schedules
    /// nothing. Empty without a program scene.
    passes: Vec<Pass>,
    /// The scene each `scene` video mix shows. A `program` or `preview` mix, and a mix
    /// not in the collection, shows the program.
    mixes: BTreeMap<VideoMixId, SceneId>,
    /// [`schedule`] of each scene a mix shows, other than the program scene.
    mix_passes: BTreeMap<SceneId, Vec<Pass>>,
    /// The cache key of each frame in `frames`, built once so a frame's uploads look
    /// them up without allocating.
    uploads: Vec<(GpuKey, SourceId)>,
    /// What the output pipeline captures for this shot: what the program's passes (or a
    /// running transition's tracks) and each mix's place, or the primary display, which
    /// fills the output, when there is no program scene.
    captures: BTreeSet<CaptureKey>,
    /// The transition running on program, which replaces the program pass: `passes`
    /// then holds only the nested scenes its tracks show.
    transition: Option<Transitioning>,
    /// [`schedule_chains`]: the order the video chains run in once per frame, with the
    /// passes of the scenes they read, made once here like `passes`.
    chain_steps: Vec<ChainStep>,
    /// The items with effects, by their chain's owner (ADR-0065).
    boxes: BTreeMap<ChainOwner, ItemBox>,
}

/// A transition as a shot draws it (ADR-0029): each item's track in drawing order and
/// the image it shows, sampled at the frame's time since `start`.
#[derive(Debug, PartialEq)]
struct Transitioning {
    start: MediaTime,
    tracks: Vec<(GpuKey, Track)>,
}

impl Default for Shot {
    /// No scene: the primary display's capture fills the output.
    fn default() -> Self {
        Self {
            canvas: Canvas::default(),
            program: None,
            scenes: BTreeMap::new(),
            frames: BTreeMap::new(),
            chains: BTreeMap::new(),
            passes: Vec::new(),
            mixes: BTreeMap::new(),
            mix_passes: BTreeMap::new(),
            uploads: Vec::new(),
            captures: captures(&[]),
            transition: None,
            chain_steps: Vec::new(),
            boxes: BTreeMap::new(),
        }
    }
}

impl Shot {
    /// The passes that draw `mix`: the program's for `None`, for a mix that shows the
    /// program or the preview (which is the program until studio mode exists), and for a
    /// mix the collection no longer has.
    fn passes_of(&self, mix: Option<&VideoMixId>) -> &[Pass] {
        mix.and_then(|id| self.mixes.get(id))
            .and_then(|scene| self.mix_passes.get(scene))
            .unwrap_or(&self.passes)
    }

    /// Whether `mix` shows the program mix, and so a running transition.
    fn shows_program(&self, mix: Option<&VideoMixId>) -> bool {
        mix.and_then(|id| self.mixes.get(id))
            .and_then(|scene| self.mix_passes.get(scene))
            .is_none()
    }

    /// The scene `mix` shows, for tests: [`Shot::passes_of`]'s root.
    #[cfg(test)]
    pub(crate) fn root_of(&self, mix: Option<&VideoMixId>) -> Option<&SceneId> {
        self.passes_of(mix).last().map(|pass| &pass.scene)
    }

    /// The program scene, for tests that follow what the render thread drew.
    #[cfg(test)]
    pub(crate) fn program(&self) -> Option<&SceneId> {
        self.program.as_ref()
    }

    /// The frame the shot shows for `source_id`, for tests.
    #[cfg(test)]
    pub(crate) fn frame(&self, source_id: &SourceId) -> Option<&Frame> {
        self.frames.get(source_id)
    }

    /// What the output pipeline captures for this shot, in order.
    pub(crate) fn captures(&self) -> &BTreeSet<CaptureKey> {
        &self.captures
    }

    /// Whether a transition is running in this shot, so what it draws changes with time.
    pub(crate) fn transitioning(&self) -> bool {
        self.transition.is_some()
    }

    /// Whether what this shot draws may change with time alone: a transition runs, or a
    /// source has a video chain, whose filters may move or hold frames (scroll, video
    /// delay). Every chain counts, which redraws a still filtered picture too.
    pub(crate) fn changes_with_time(&self) -> bool {
        self.transitioning() || !self.chains.is_empty()
    }

    /// How many sources have a video chain in this shot, for tests.
    #[cfg(test)]
    pub(crate) fn chain_count(&self) -> usize {
        self.chains.len()
    }

    /// The video chain of `source_id` in this shot, for tests.
    #[cfg(test)]
    pub(crate) fn chain(&self, source_id: &SourceId) -> Option<&Chain> {
        self.chains.get(&ChainOwner::Source(source_id.clone()))
    }

    /// The effects chain of item `item_id` as scene `scene_id` draws it, for tests.
    #[cfg(test)]
    pub(crate) fn item_chain(&self, scene_id: &SceneId, item_id: &ItemId) -> Option<&Chain> {
        self.chains
            .get(&ChainOwner::Item(scene_id.clone(), item_id.clone()))
    }

    /// The video chain of the scene `scene_id` in this shot, for tests.
    #[cfg(test)]
    pub(crate) fn scene_chain(&self, scene_id: &SceneId) -> Option<&Chain> {
        self.chains.get(&ChainOwner::Scene(scene_id.clone()))
    }

    /// How many tracks the running transition draws, or `None` without one, for tests.
    #[cfg(test)]
    pub(crate) fn transition_tracks(&self) -> Option<usize> {
        self.transition.as_ref().map(|t| t.tracks.len())
    }

    /// The running transition's tracks in drawing order, or `None` without one, for
    /// tests.
    #[cfg(test)]
    pub(crate) fn tracks(&self) -> Option<Vec<Track>> {
        self.transition
            .as_ref()
            .map(|t| t.tracks.iter().map(|(_, track)| *track).collect())
    }

    /// What scene `id` places, bottom first: each item with its placement and opacity,
    /// or `None` when the shot doesn't draw the scene, for tests.
    #[cfg(test)]
    pub(crate) fn placed(&self, id: &SceneId) -> Option<Vec<(ItemId, Placement, f32)>> {
        self.scenes.get(id).map(|placed| {
            placed
                .iter()
                .map(|p| (p.item.clone(), p.placement, p.opacity))
                .collect()
        })
    }
}

/// Two shots are equal when they draw the same thing: the passes and keys follow from
/// the rest.
impl PartialEq for Shot {
    fn eq(&self, other: &Self) -> bool {
        self.canvas == other.canvas
            && self.program == other.program
            && self.mixes == other.mixes
            && self.scenes == other.scenes
            && self.frames == other.frames
            && self.chains == other.chains
            && self.captures == other.captures
            && self.transition == other.transition
            && self.boxes == other.boxes
    }
}

/// Copies `graph`'s first scene, as the program scene, and the scenes it nests, and
/// schedules them. Items show the frames in `frames`.
#[cfg(test)]
pub(crate) fn plan(graph: &Graph, frames: &BTreeMap<SourceId, Frame>) -> Shot {
    let first = graph
        .state()
        .scenes()
        .first()
        .map(|scene| scene.id().clone());
    plan_with(
        graph,
        first.as_ref(),
        None,
        &mut |id| frames.get(id).map(|f| Look::Frame(f.clone(), None)),
        &FilterAssets::default(),
    )
    .shot
}

/// What [`plan_with`] returns.
pub(crate) struct Planning {
    pub(crate) shot: Shot,
    /// When the shot should be planned again: the earliest time a frame it shows is
    /// replaced, or the running transition ends.
    pub(crate) wake: Option<MediaTime>,
    /// Whether the running transition it was given has ended, so the shot shows the
    /// program scene alone and the studio can let the transition go.
    pub(crate) finished: bool,
    /// How long the transition the shot draws lasts, in milliseconds from its start.
    pub(crate) end_ms: Option<f32>,
}

/// Copies the program scene `program` (ADR-0030), the scene each `scene` video mix
/// shows, and the scenes they nest out of `graph`, and schedules them, asking `look`
/// what each source an item they reach places shows: its frame and when that frame is
/// next replaced, or what the pipeline captures for it. A video filter that reads an
/// asset gets it from `assets`, the ones loaded now.
///
/// With a `running` transition (ADR-0029) toward `program` that has not ended at `now`,
/// the shot draws the transition instead of the program scene: its tracks, planned from
/// what it starts from and the program scene as the collection holds them now, and the
/// nested scenes they show. A video mix that shows the program shows the transition too.
pub(crate) fn plan_with(
    graph: &Graph,
    program: Option<&SceneId>,
    running: Option<(&Running, MediaTime)>,
    look: &mut LookUp<'_>,
    assets: &FilterAssets,
) -> Planning {
    let state = graph.state();
    let program = program.cloned();
    let mut shot = Shot {
        canvas: state.canvas(),
        program: program.clone(),
        ..Shot::default()
    };
    let mut planner = Planner {
        state,
        look,
        assets,
        wake: None,
    };
    let mut finished = running.is_some();
    let mut end_ms = None;
    if let Some((running, now)) =
        running.filter(|(running, _)| Some(&running.to) == program.as_ref())
    {
        let planned = planner.transition(running, &mut shot);
        let end = end_time(running.start, planned.end_ms);
        if end.is_some_and(|end| now < end) {
            finished = false;
            end_ms = Some(planned.end_ms);
            planner.wake = earliest(planner.wake, end);
            let tracks = planned
                .tracks
                .into_iter()
                .map(|(visual, track)| (key(&visual.image, &shot), track))
                .collect();
            shot.transition = Some(Transitioning {
                start: running.start,
                tracks,
            });
            shot.passes = schedule_tracks(&shot);
        } else {
            // Ended: plan the program scene alone, without the transition's leftovers.
            shot.scenes.clear();
            shot.frames.clear();
            shot.chains.clear();
            planner.wake = None;
        }
    }
    if shot.transition.is_none()
        && let Some(id) = &program
    {
        let mut path = BTreeSet::new();
        planner.collect(id, &mut shot, &mut path);
        shot.passes = schedule(&shot, id);
    }
    for mix in state.video_mixes() {
        if let Some(scene_id) = mix.mix().scene() {
            shot.mixes.insert(mix.id().clone(), scene_id.clone());
        }
    }
    let roots: BTreeSet<SceneId> = shot
        .mixes
        .values()
        // While a transition replaces the program pass, a mix of the program scene
        // draws that scene from passes of its own.
        .filter(|scene| shot.transition.is_some() || Some(*scene) != program.as_ref())
        .cloned()
        .collect();
    for root in roots {
        let mut path = BTreeSet::new();
        planner.collect(&root, &mut shot, &mut path);
        let passes = schedule(&shot, &root);
        shot.mix_passes.insert(root, passes);
    }
    shot.uploads = shot
        .frames
        .keys()
        .map(|id| (GpuKey::Frame(id.clone()), id.clone()))
        .collect();
    shot.captures = match &shot.transition {
        Some(transition) => placed_captures(shot.passes.iter())
            .chain(transition.tracks.iter().filter_map(|(key, _)| match key {
                GpuKey::Capture(spec) => Some(spec.clone()),
                _ => None,
            }))
            .collect(),
        None => captures(&shot.passes),
    };
    shot.captures
        .extend(placed_captures(shot.mix_passes.values().flatten()));
    // An item's effects read what the item shows, though the layers show their output.
    shot.captures
        .extend(shot.boxes.values().filter_map(|boxed| match &boxed.source {
            SourceRef::Capture(spec) => Some(spec.clone()),
            _ => None,
        }));
    // A chain over a capture reads it, though the layers show the chain's output.
    shot.captures
        .extend(shot.chains.values().filter_map(|chain| match &chain.input {
            GpuKey::Capture(spec) => Some(spec.clone()),
            _ => None,
        }));
    shot.chain_steps = schedule_chains(&shot);
    let read = placed_captures(shot.chain_steps.iter().filter_map(|step| match step {
        ChainStep::Pass(pass) => Some(pass),
        ChainStep::Run(_) | ChainStep::Box(_) => None,
    }))
    .collect::<Vec<_>>();
    shot.captures.extend(read);
    Planning {
        shot,
        wake: planner.wake,
        finished,
        end_ms,
    }
}

/// What is on screen at `now` while `running` runs toward `program`, as the starting
/// point of a switch that retargets it (ADR-0029 §7): the items its tracks still draw
/// then, sampled with the function the render thread draws them with.
pub(crate) fn on_screen(
    graph: &Graph,
    running: &Running,
    now: MediaTime,
    look: &mut LookUp<'_>,
    assets: &FilterAssets,
) -> Vec<Visual> {
    let state = graph.state();
    let mut shot = Shot {
        canvas: state.canvas(),
        ..Shot::default()
    };
    let mut planner = Planner {
        state,
        look,
        assets,
        wake: None,
    };
    let planned = planner.transition(running, &mut shot);
    planned.on_screen(running.elapsed_ms(now), canvas_size(state.canvas()))
}

fn canvas_size(canvas: Canvas) -> [f32; 2] {
    [canvas.width as f32, canvas.height as f32]
}

/// When a transition that started at `start` and lasts `end_ms` ends, on the same clock.
fn end_time(start: MediaTime, end_ms: f32) -> Option<MediaTime> {
    let ms = MediaTime::new(
        end_ms.ceil() as i64,
        jstream_media::Timebase::new(1, 1000).ok()?,
    );
    let ms = ms.rescale(start.timebase(), Rounding::Up).ok()?;
    start.checked_add(ms).ok()
}

fn earliest(wake: Option<MediaTime>, at: Option<MediaTime>) -> Option<MediaTime> {
    match (wake, at) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The cached image a visual shows: a frame's upload, the capture's, a nested scene's
/// layer one level below the program, or its chain's output when `shot` runs one.
fn key(image: &SourceRef, shot: &Shot) -> GpuKey {
    match image {
        SourceRef::Frame(id) => GpuKey::Frame(id.clone()),
        SourceRef::Capture(spec) => GpuKey::Capture(spec.clone()),
        SourceRef::Scene(id) => {
            let owner = ChainOwner::Scene(id.clone());
            if shot.chains.contains_key(&owner) {
                GpuKey::Filtered(owner)
            } else {
                GpuKey::Layer(id.clone(), 1)
            }
        }
        SourceRef::Filtered(id) => GpuKey::Filtered(ChainOwner::Source(id.clone())),
        SourceRef::Boxed(scene, item) => {
            GpuKey::Filtered(ChainOwner::Item(scene.clone(), item.clone()))
        }
    }
}

/// The passes a transition shot draws before its tracks: every nested scene a track
/// shows, at depth 1, and the scenes they nest. A scene with a chain is drawn before
/// any mix, with the chains ([`schedule_chains`]).
fn schedule_tracks(shot: &Shot) -> Vec<Pass> {
    let mut passes = Vec::new();
    let mut done = BTreeSet::new();
    let mut path = BTreeSet::new();
    for (key, _) in shot.transition.iter().flat_map(|t| &t.tracks) {
        if let GpuKey::Layer(scene, depth) = key {
            visit(shot, scene, *depth, &mut path, &mut done, &mut passes);
        }
    }
    passes
}

/// What the passes place from the pipeline's capture, or the primary display, which
/// fills the output, without a program scene.
fn captures(passes: &[Pass]) -> BTreeSet<CaptureKey> {
    if passes.is_empty() {
        return BTreeSet::from([CaptureKey::primary()]);
    }
    placed_captures(passes.iter()).collect()
}

/// What `passes` place from the pipeline's capture.
fn placed_captures<'a>(
    passes: impl Iterator<Item = &'a Pass> + 'a,
) -> impl Iterator<Item = CaptureKey> + 'a {
    passes
        .flat_map(|pass| &pass.layers)
        .filter_map(|layer| match &layer.image {
            GpuKey::Capture(spec) => Some(spec.clone()),
            _ => None,
        })
}

/// What a source shows, as [`plan_with`] asks for it.
pub(crate) enum Look {
    /// Its frame now, and when that frame is next replaced.
    Frame(Frame, Option<MediaTime>),
    /// What the output pipeline captures for it.
    Capture(CaptureKey),
}

/// What [`plan_with`] asks for each source it reaches; `None` shows nothing.
pub(crate) type LookUp<'a> = dyn FnMut(&SourceId) -> Option<Look> + 'a;

struct Planner<'a, 'f> {
    state: &'a State,
    look: &'f mut LookUp<'f>,
    /// The assets loaded for video filters.
    assets: &'a FilterAssets,
    /// The earliest time a frame in the shot is replaced.
    wake: Option<MediaTime>,
}

impl Planner<'_, '_> {
    /// Plans `running`: what it starts from, paired with the program scene's items. The
    /// frames and nested scenes either side shows go into `out`.
    fn transition(&mut self, running: &Running, out: &mut Shot) -> Planned {
        let flatten = running.run.pairs;
        let from = match &running.from {
            From::Scene(scene) => self.endpoint(scene, flatten, out),
            From::OnScreen(items) => items
                .iter()
                .filter(|visual| self.shows(&visual.image, out))
                .cloned()
                .collect(),
        };
        let to = self.endpoint(&running.to, flatten, out);
        transition::plan(&from, &to, &running.run, running.by_item)
    }

    /// What a transition shows of scene `id` at one end: its items as
    /// [`Self::visuals`] gives them or, for a scene with a chain, its filtered picture
    /// as one visual over the canvas, which pairs with nothing (ADR-0065 §6): Fade
    /// crossfades it and Move falls back to that.
    fn endpoint(&mut self, id: &SceneId, flatten: bool, out: &mut Shot) -> Vec<Visual> {
        if self.scene_chain(id).is_none() {
            return self.visuals(id, flatten, out);
        }
        self.collect(id, out, &mut BTreeSet::new());
        let canvas = self.state.canvas();
        let Ok(geometry) = ItemGeometry::covering_canvas(canvas.width, canvas.height) else {
            return Vec::new();
        };
        vec![Visual {
            image: SourceRef::Scene(id.clone()),
            item_id: ItemId::new(""),
            source_id: SourceId::new(""),
            at: Endpoint {
                geometry,
                crop: Crop::NONE,
                opacity: OPAQUE,
            },
            size: Some([canvas.width, canvas.height]),
            settings: ItemTransition {
                glide: Glide::Never,
                ..ItemTransition::default()
            },
            effects: Vec::new(),
        }]
    }

    /// The video chain of scene `id` as the collection holds it, over its composite: a
    /// keyframe scene's is its parent's (ADR-0024). `None` when no filter of it runs.
    fn scene_chain(&self, id: &SceneId) -> Option<Chain> {
        let scene = self.state.scene(id)?;
        Chain::of_filters(
            &ChainOwner::Scene(id.clone()),
            scene_filters(self.state, scene),
            GpuKey::Composite(id.clone()),
            self.assets,
        )
    }

    /// Puts `chain`, `owner`'s, in `out`, with the scenes its filters read planned like
    /// nested scenes, and each filter that reads a scene with a chain of its own pointed
    /// at that chain's output.
    fn add_chain(&mut self, owner: ChainOwner, chain: Chain, out: &mut Shot) {
        let scenes: Vec<SceneId> = chain.scenes().cloned().collect();
        out.chains.insert(owner.clone(), chain);
        for scene in &scenes {
            self.collect(scene, out, &mut BTreeSet::new());
        }
        let filtered: BTreeSet<SceneId> = scenes
            .into_iter()
            .filter(|scene| out.chains.contains_key(&ChainOwner::Scene(scene.clone())))
            .collect();
        if let Some(chain) = out.chains.get_mut(&owner) {
            chain.read_filtered(|scene| filtered.contains(scene));
        }
    }

    /// Whether `image` can be drawn now, putting its frame or scene in `out`.
    fn shows(&mut self, image: &SourceRef, out: &mut Shot) -> bool {
        match image {
            SourceRef::Frame(id) => self.look(id, out).is_some(),
            SourceRef::Capture(_) => true,
            // The chain as the collection holds it now; a source that has since lost
            // every filter that runs is left out.
            SourceRef::Filtered(id) => {
                let state = self.state;
                state.source(id).is_some_and(|source| {
                    matches!(self.shown(source, out), Some(SourceRef::Filtered(_)))
                })
            }
            SourceRef::Scene(id) => {
                self.collect(id, out, &mut BTreeSet::new());
                true
            }
            // The item's effects as the collection holds them now.
            SourceRef::Boxed(scene, item) => self.reboxed(scene, item, out),
        }
    }

    /// Plans the effects of item `item_id` as scene `scene_id` draws it, for a
    /// transition that retargets one with it on screen: whether they still run.
    fn reboxed(&mut self, scene_id: &SceneId, item_id: &ItemId, out: &mut Shot) -> bool {
        let state = self.state;
        let Some(scene) = state.scene(scene_id) else {
            return false;
        };
        let Some(item) = state
            .shown_items(scene)
            .into_iter()
            .find(|item| item.as_ref().id() == item_id)
        else {
            return false;
        };
        let item = item.as_ref();
        let Some(source) = state.source(item.source_id()) else {
            return false;
        };
        let image = match source.kind() {
            SourceKind::Scene { scene_id } => {
                self.collect(scene_id, out, &mut BTreeSet::new());
                SourceRef::Scene(scene_id.clone())
            }
            _ => match self.shown(source, out) {
                Some(image) => image,
                None => return false,
            },
        };
        self.boxed(scene_id, item, &image, out).is_some()
    }

    /// Plans the effects of `item`, as scene `scene` draws it over `source`, into `out`:
    /// their chain over the item's box, and the box (ADR-0065 §5). `None` when no effect
    /// of it runs, or when its box isn't known (a legacy transform over a capture,
    /// whose size isn't known when planning), which draws it as if it had none.
    fn boxed(
        &mut self,
        scene: &SceneId,
        item: &GraphItem,
        source: &SourceRef,
        out: &mut Shot,
    ) -> Option<Boxed> {
        let owner = ChainOwner::Item(scene.clone(), item.id().clone());
        if let Some(planned) = out.boxes.get(&owner) {
            return Some(planned.boxed);
        }
        let chain = Chain::of_filters(
            &owner,
            item.effects(),
            GpuKey::ItemBox(scene.clone(), item.id().clone()),
            self.assets,
        )?;
        let canvas = self.state.canvas();
        let output = |chain: Option<&Chain>, [width, height]: [u32; 2]| {
            let (w, h) = chain.map_or((width, height), |chain| chain.output_size(width, height));
            [w, h]
        };
        let input = match source {
            SourceRef::Frame(id) => out.frames.get(id).map(|frame| frame.extent),
            SourceRef::Filtered(id) => out.frames.get(id).map(|frame| {
                output(
                    out.chains.get(&ChainOwner::Source(id.clone())),
                    frame.extent,
                )
            }),
            SourceRef::Scene(id) => Some(output(
                out.chains.get(&ChainOwner::Scene(id.clone())),
                [canvas.width, canvas.height],
            )),
            SourceRef::Capture(_) | SourceRef::Boxed(..) => None,
        };
        let geometry = match item.placement() {
            GraphPlacement::Geometry(geometry) => *geometry,
            GraphPlacement::Legacy(transform) => {
                let Some([width, height]) = input else {
                    tracing::debug!(
                        item = %item.id(),
                        "a legacy transform over a capture has no known box; its effects are bypassed"
                    );
                    return None;
                };
                ItemGeometry::from_legacy(*transform, *item.crop(), width, height)
                    .ok()??
                    .geometry
            }
            _ => return None,
        };
        let boxed = Boxed::new(&geometry, chain.outset(), input)?;
        out.boxes.insert(
            owner.clone(),
            ItemBox {
                item: item.id().clone(),
                source: source.clone(),
                placement: Placement::Geometry(geometry),
                crop: crop(item.crop()),
                boxed,
            },
        );
        self.add_chain(owner, chain, out);
        Some(boxed)
    }

    /// The visible items of scene `id`, bottom first, as transition endpoints, each at
    /// its own opacity. With `flatten`, a nested scene placed over the whole canvas,
    /// uncropped and opaque, is replaced by its own items (ADR-0029 §4): it then draws
    /// exactly what its layer would, so its items can pair one by one. Nesting stops at
    /// [`MAX_NESTING`] levels.
    fn visuals(&mut self, id: &SceneId, flatten: bool, out: &mut Shot) -> Vec<Visual> {
        let mut visuals = Vec::new();
        let mut path = BTreeSet::new();
        self.visuals_into(id, flatten, 0, &mut path, out, &mut visuals);
        visuals
    }

    fn visuals_into(
        &mut self,
        id: &SceneId,
        flatten: bool,
        depth: usize,
        path: &mut BTreeSet<SceneId>,
        out: &mut Shot,
        visuals: &mut Vec<Visual>,
    ) {
        let state = self.state;
        let Some(scene) = state.scene(id) else {
            return;
        };
        if !path.insert(id.clone()) {
            return;
        }
        let canvas = state.canvas();
        for item in state.shown_items(scene) {
            let item = item.as_ref();
            if !item.visible() {
                continue;
            }
            let Some(source) = state.source(item.source_id()) else {
                continue;
            };
            let (image, size) = match source.kind() {
                SourceKind::Scene { scene_id } => {
                    // A scene with a chain is one picture (ADR-0065 §6).
                    if flatten
                        && depth + 1 < MAX_NESTING
                        && covers(item, canvas)
                        && item.opacity().get() >= OPAQUE
                        && self.scene_chain(scene_id).is_none()
                        && !item.effects().iter().any(|effect| effect.enabled())
                    {
                        self.visuals_into(scene_id, flatten, depth + 1, path, out, visuals);
                        continue;
                    }
                    self.collect(scene_id, out, &mut BTreeSet::new());
                    (
                        SourceRef::Scene(scene_id.clone()),
                        Some([canvas.width, canvas.height]),
                    )
                }
                _ => match self.shown(source, out) {
                    Some(SourceRef::Frame(id)) => {
                        let size = out.frames.get(&id).map(|frame| frame.extent);
                        (SourceRef::Frame(id), size)
                    }
                    // A filtered frame is the size the chain draws for it.
                    Some(SourceRef::Filtered(id)) => {
                        let chain = out.chains.get(&ChainOwner::Source(id.clone()));
                        let size = out.frames.get(&id).zip(chain).map(|(frame, chain)| {
                            let [width, height] = frame.extent;
                            let (w, h) = chain.output_size(width, height);
                            [w, h]
                        });
                        (SourceRef::Filtered(id), size)
                    }
                    // A capture's size isn't known when planning: the canvas stands in.
                    Some(capture @ SourceRef::Capture(_)) => (capture, None),
                    Some(SourceRef::Scene(_) | SourceRef::Boxed(..)) | None => continue,
                },
            };
            let Some(geometry) = endpoint_geometry(item, size, canvas) else {
                continue;
            };
            // An item with effects is its effects' layer over its grown box, uncropped.
            let boxed = self.boxed(id, item, &image, out);
            let (image, geometry, cut, size, effects) = match boxed {
                Some(boxed) => {
                    let owner = ChainOwner::Item(id.clone(), item.id().clone());
                    let look = out.chains.get(&owner).map(Chain::look).unwrap_or_default();
                    let Some(around) = boxed.around(&geometry) else {
                        continue;
                    };
                    (
                        SourceRef::Boxed(id.clone(), item.id().clone()),
                        around,
                        Crop::NONE,
                        Some(boxed.layer),
                        look,
                    )
                }
                None => (image, geometry, crop(item.crop()), size, Vec::new()),
            };
            visuals.push(Visual {
                image,
                item_id: item.id().clone(),
                source_id: item.source_id().clone(),
                at: Endpoint {
                    geometry,
                    crop: cut,
                    opacity: item.opacity().get(),
                },
                size,
                settings: *item.transition(),
                effects,
            });
        }
        path.remove(id);
    }

    fn collect(&mut self, id: &SceneId, out: &mut Shot, path: &mut BTreeSet<SceneId>) {
        if out.scenes.contains_key(id) || !path.insert(id.clone()) {
            return;
        }
        let state = self.state;
        let mut placed = Vec::new();
        if let Some(scene) = state.scene(id) {
            // A keyframe scene draws its parent's items at their effective values.
            for item in state.shown_items(scene) {
                let item = item.as_ref();
                if !item.visible() {
                    continue;
                }
                let Some(source) = state.source(item.source_id()) else {
                    continue;
                };
                let source = match source.kind() {
                    SourceKind::Scene { scene_id } => {
                        self.collect(scene_id, out, path);
                        SourceRef::Scene(scene_id.clone())
                    }
                    _ => match self.shown(source, out) {
                        Some(source) => source,
                        None => continue,
                    },
                };
                let effects = self.boxed(id, item, &source, out);
                placed.push(Placed {
                    item: item.id().clone(),
                    source,
                    placement: placement(item.placement()),
                    crop: crop(item.crop()),
                    opacity: item.opacity().get(),
                    effects,
                });
            }
        }
        out.scenes.insert(id.clone(), placed);
        if let Some(chain) = self.scene_chain(id) {
            self.add_chain(ChainOwner::Scene(id.clone()), chain, out);
        }
        path.remove(id);
    }

    /// What `source_id` shows. Its frame goes in `out`, asked for once per shot.
    fn look(&mut self, source_id: &SourceId, out: &mut Shot) -> Option<SourceRef> {
        if out.frames.contains_key(source_id) {
            return Some(SourceRef::Frame(source_id.clone()));
        }
        match (self.look)(source_id)? {
            Look::Frame(frame, next) => {
                if let Some(next) = next {
                    self.wake = Some(self.wake.map_or(next, |wake| wake.min(next)));
                }
                out.frames.insert(source_id.clone(), frame);
                Some(SourceRef::Frame(source_id.clone()))
            }
            Look::Capture(spec) => Some(SourceRef::Capture(spec)),
        }
    }

    /// What `source`, which is not a scene, shows: [`Self::look`]'s frame or capture,
    /// after the source's video chain when a filter of it runs. The chain goes in `out`,
    /// once per shot.
    fn shown(&mut self, source: &Source, out: &mut Shot) -> Option<SourceRef> {
        let id = source.id();
        let image = self.look(id, out)?;
        let owner = ChainOwner::Source(id.clone());
        if out.chains.contains_key(&owner) {
            return Some(SourceRef::Filtered(id.clone()));
        }
        match Chain::of(source, key(&image, out), self.assets) {
            Some(chain) => {
                // What a `scene_mask` reads is planned like a nested scene.
                self.add_chain(owner, chain, out);
                Some(SourceRef::Filtered(id.clone()))
            }
            None => Some(image),
        }
    }
}

/// What a drawing compositor keeps between frames in its [`ResourceCache`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GpuKey {
    /// The upload of a source's stored frame.
    Frame(SourceId),
    /// The upload or import of what the pipeline captures.
    Capture(CaptureKey),
    /// The layer a nested scene is composited into, at its depth.
    Layer(SceneId, usize),
    /// The program scene at the canvas size, when the output is another size.
    Program,
    /// The output of a source's or a scene's video chain: what its items show.
    Filtered(ChainOwner),
    /// The output of the filter at that index of a video chain, which the next one
    /// reads. The last filter draws into [`GpuKey::Filtered`] instead.
    Chain(ChainOwner, usize),
    /// The composite of a scene with a video chain, which the chain runs over
    /// (ADR-0065): drawn once per frame, before any mix.
    Composite(SceneId),
    /// The layer an item with effects is drawn into before they run (ADR-0065 §5), as
    /// that scene draws it.
    ItemBox(SceneId, ItemId),
    /// One step of the privacy blur at that width and height: the output itself, then
    /// halved until it is at most [`PRIVACY_SIDE`] on its longest side (ADR-0027 §4).
    Privacy(u32, u32),
}

/// The depth a scene that a video filter reads (`scene_mask`) is drawn at: as a scene
/// nested one level down, so what it nests counts toward [`MAX_NESTING`] the same way.
const REFERENCE_DEPTH: usize = 1;

impl GpuKey {
    /// The layer `scene` is drawn into for the video filters that read it.
    pub(crate) fn reference(scene: &SceneId) -> Self {
        Self::Layer(scene.clone(), REFERENCE_DEPTH)
    }
}

/// One step of the work a frame does once, before any mix draws: a source's video
/// chain, or a scene pass a chain reads, in an order where each comes after what it
/// reads ([`schedule_chains`]).
#[derive(Debug, PartialEq)]
enum ChainStep {
    /// A scene a `scene_mask` reads, or a scene that one nests.
    Pass(Pass),
    /// An item with effects, drawn into its [`GpuKey::ItemBox`] before they run.
    Box(ChainOwner),
    /// The video chain of a source or a scene.
    Run(ChainOwner),
}

/// What [`compose`] draws: which mix, and whether the privacy stage applies.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct View<'a> {
    /// The mix to draw; `None` for the program.
    pub(crate) mix: Option<&'a VideoMixId>,
    /// `None` off the output path (`video.snapshot`, the editing view), where there is
    /// no privacy stage. On it, `Some` with whether the privacy screen is on: its blur
    /// layers are kept either way, so turning it on allocates nothing.
    pub(crate) privacy: Option<bool>,
}

/// The longest side the privacy blur halves the picture down to before scaling it back
/// up: a 1920×1080 output becomes 30×17, which hides faces and text.
const PRIVACY_SIDE: u32 = 32;

/// One composite in a frame: a scene at a nesting depth (the program scene is 0).
#[derive(Debug, PartialEq)]
struct Pass {
    scene: SceneId,
    depth: usize,
    /// Where the pass draws: its [`GpuKey::Layer`]. The last pass, the program scene,
    /// draws to the output instead.
    key: GpuKey,
    layers: Vec<Layer>,
}

/// One item of a [`Pass`].
#[derive(Debug, PartialEq)]
struct Layer {
    /// The item it draws, whose live values override `placement` and `crop`.
    item: ItemId,
    /// The cached image the item shows: a frame's upload, the capture's, or the layer an
    /// earlier pass drew.
    image: GpuKey,
    placement: Placement,
    crop: Crop,
    opacity: f32,
    /// The box of an item whose effects run: `image` is their output, placed over the
    /// box grown by their outset, uncropped.
    boxed: Option<Boxed>,
}

impl Shot {
    /// How much each source whose frame a layer draws is enlarged where it is shown
    /// most, in canvas pixels per pixel of the frame's extent: what its source should
    /// draw at to stay sharp (ADR-0073). It is measured on every pass, the program's and
    /// the video mixes', from the placements the shot holds, not a gesture's live
    /// values, so a drag is measured once it commits. A source a video chain runs over
    /// counts as 1, since its filters' settings are in the frame's pixels. A source the
    /// shot draws only in a transition's tracks, or not at all, isn't listed.
    pub(crate) fn scales(&self) -> BTreeMap<SourceId, f32> {
        let mut scales: BTreeMap<SourceId, f32> = BTreeMap::new();
        for layer in self
            .passes
            .iter()
            .chain(self.mix_passes.values().flatten())
            .flat_map(|pass| &pass.layers)
        {
            let GpuKey::Frame(id) = &layer.image else {
                continue;
            };
            let Some(frame) = self.frames.get(id) else {
                continue;
            };
            let scale = magnification(layer.placement, layer.crop, frame.extent);
            let most = scales.entry(id.clone()).or_insert(0.0);
            *most = most.max(scale);
        }
        // An item's effects draw its frame into their layer, as the item would.
        for boxed in self.boxes.values() {
            let SourceRef::Frame(id) = &boxed.source else {
                continue;
            };
            let Some(frame) = self.frames.get(id) else {
                continue;
            };
            let scale = magnification(boxed.placement, boxed.crop, frame.extent);
            let most = scales.entry(id.clone()).or_insert(0.0);
            *most = most.max(scale);
        }
        for (owner, chain) in &self.chains {
            if let ChainOwner::Source(id) = owner
                && chain.input == GpuKey::Frame(id.clone())
            {
                scales.insert(id.clone(), 1.0);
            }
        }
        scales
    }
}

/// How many canvas pixels each way one pixel of an image of `extent`, cropped by
/// `crop`, covers where `placement` puts it, along the axis it is enlarged most. 0 when
/// it draws nothing.
fn magnification(placement: Placement, crop: Crop, extent: [u32; 2]) -> f32 {
    let scale = match placement {
        Placement::Legacy(transform) => transform.scale[0].abs().max(transform.scale[1].abs()),
        Placement::Geometry(geometry) => {
            let cut = ProtocolCrop {
                left: crop.left,
                top: crop.top,
                right: crop.right,
                bottom: crop.bottom,
            };
            let width = extent[0].saturating_sub(crop.left.saturating_add(crop.right));
            let height = extent[1].saturating_sub(crop.top.saturating_add(crop.bottom));
            match geometry.content(extent[0], extent[1], cut) {
                Ok(Some(content)) if width > 0 && height > 0 => {
                    (content.width / width as f32).max(content.height / height as f32)
                }
                _ => 0.0,
            }
        }
    };
    if scale.is_finite() { scale } else { 0.0 }
}

/// The composites a frame makes for the scene `program`, in order: every nested scene
/// before the scenes that place it, the program scene last.
///
/// A scene is rendered once per depth it is reached at, however often it is placed
/// there. A scene source more than [`MAX_NESTING`] levels below the program scene, or
/// one that shows a scene already being drawn further up (a cycle, which the graph
/// rejects), draws nothing. A scene with a video chain (ADR-0065) isn't composited
/// here: the chain phase draws it once per frame ([`schedule_chains`]), and the passes
/// here place its chain's output, the program scene's over the whole canvas.
fn schedule(shot: &Shot, program: &SceneId) -> Vec<Pass> {
    let owner = ChainOwner::Scene(program.clone());
    if shot.chains.contains_key(&owner) {
        let canvas = shot.canvas;
        let layers = ItemGeometry::covering_canvas(canvas.width, canvas.height)
            .map(|geometry| Layer {
                // No item: a live value never moves the whole picture.
                item: ItemId::new(""),
                image: GpuKey::Filtered(owner),
                placement: Placement::Geometry(geometry),
                crop: Crop::NONE,
                opacity: OPAQUE,
                boxed: None,
            })
            .into_iter()
            .collect();
        return vec![Pass {
            scene: program.clone(),
            depth: 0,
            key: GpuKey::Layer(program.clone(), 0),
            layers,
        }];
    }
    let mut passes = Vec::new();
    let mut done = BTreeSet::new();
    let mut path = BTreeSet::new();
    visit(shot, program, 0, &mut path, &mut done, &mut passes);
    passes
}

/// Where [`visit`] puts the passes it schedules.
trait Passes {
    /// Takes `pass`, after every pass it reads.
    fn pass(&mut self, pass: Pass);

    /// Called when a pass places the output of `owner`'s video chain, before the pass
    /// is taken. A mix's passes ignore it: every chain runs before any mix draws.
    fn filtered(&mut self, _shot: &Shot, _owner: &ChainOwner, _done: &mut BTreeSet<GpuKey>) {}
}

impl Passes for Vec<Pass> {
    fn pass(&mut self, pass: Pass) {
        self.push(pass);
    }
}

/// The work a frame does once, before any mix draws, in order: each video chain after
/// what it reads, and each pass after the chains of what it places and the scenes it
/// nests. A source's chain reads its frame, and a scene's its composite, drawn at
/// [`REFERENCE_DEPTH`] into [`GpuKey::Composite`]. A scene a chain's filter reads
/// (`scene_mask`) is drawn at [`REFERENCE_DEPTH`] into [`GpuKey::reference`], or read
/// from its own chain's output when it has one. The graph refuses a cycle through a
/// filter's scene reference (ADR-0026); were one there, the chain that closes it would
/// read its owner's output from the frame before.
fn schedule_chains(shot: &Shot) -> Vec<ChainStep> {
    let mut order = ChainOrder::default();
    let mut done = BTreeSet::new();
    for owner in shot.chains.keys() {
        order.chain(shot, owner, &mut done);
    }
    order.steps
}

#[derive(Default)]
struct ChainOrder {
    steps: Vec<ChainStep>,
    /// The chains already in `steps`.
    ran: BTreeSet<ChainOwner>,
    /// The chains being ordered, whose scenes are being scheduled.
    open: BTreeSet<ChainOwner>,
}

impl ChainOrder {
    fn chain(&mut self, shot: &Shot, owner: &ChainOwner, done: &mut BTreeSet<GpuKey>) {
        if self.ran.contains(owner) || !self.open.insert(owner.clone()) {
            return;
        }
        if let Some(chain) = shot.chains.get(owner) {
            // An item's effects read what the item shows.
            if let Some(boxed) = shot.boxes.get(owner) {
                match &boxed.source {
                    SourceRef::Filtered(source) => {
                        self.chain(shot, &ChainOwner::Source(source.clone()), done);
                    }
                    SourceRef::Scene(inner) => {
                        let read = ChainOwner::Scene(inner.clone());
                        if shot.chains.contains_key(&read) {
                            self.chain(shot, &read, done);
                        } else {
                            visit(
                                shot,
                                inner,
                                REFERENCE_DEPTH,
                                &mut BTreeSet::new(),
                                done,
                                self,
                            );
                        }
                    }
                    SourceRef::Frame(_) | SourceRef::Capture(_) | SourceRef::Boxed(..) => {}
                }
            }
            for scene in chain.scenes() {
                let read = ChainOwner::Scene(scene.clone());
                if shot.chains.contains_key(&read) {
                    self.chain(shot, &read, done);
                } else {
                    visit(
                        shot,
                        scene,
                        REFERENCE_DEPTH,
                        &mut BTreeSet::new(),
                        done,
                        self,
                    );
                }
            }
            if let ChainOwner::Scene(scene) = owner {
                visit_into(
                    shot,
                    (scene, REFERENCE_DEPTH, GpuKey::Composite(scene.clone())),
                    &mut BTreeSet::new(),
                    done,
                    self,
                );
            }
            if shot.boxes.contains_key(owner) {
                self.steps.push(ChainStep::Box(owner.clone()));
            }
        }
        self.open.remove(owner);
        self.ran.insert(owner.clone());
        self.steps.push(ChainStep::Run(owner.clone()));
    }
}

impl Passes for ChainOrder {
    fn pass(&mut self, pass: Pass) {
        self.steps.push(ChainStep::Pass(pass));
    }

    fn filtered(&mut self, shot: &Shot, owner: &ChainOwner, done: &mut BTreeSet<GpuKey>) {
        self.chain(shot, owner, done);
    }
}

/// Schedules scene `id` at `depth` into its [`GpuKey::Layer`], after what it reads.
fn visit(
    shot: &Shot,
    id: &SceneId,
    depth: usize,
    path: &mut BTreeSet<SceneId>,
    done: &mut BTreeSet<GpuKey>,
    passes: &mut impl Passes,
) -> GpuKey {
    let key = GpuKey::Layer(id.clone(), depth);
    visit_into(shot, (id, depth, key.clone()), path, done, passes);
    key
}

/// Schedules scene `id` at `depth` into `key`, once, after the passes and chains of
/// what it places. A nested scene with a chain is placed as its chain's output.
fn visit_into(
    shot: &Shot,
    (id, depth, key): (&SceneId, usize, GpuKey),
    path: &mut BTreeSet<SceneId>,
    done: &mut BTreeSet<GpuKey>,
    passes: &mut impl Passes,
) {
    if !done.insert(key.clone()) {
        return;
    }
    path.insert(id.clone());
    let mut layers = Vec::new();
    for placed in shot.scenes.get(id).map(Vec::as_slice).unwrap_or(&[]) {
        let image = match &placed.source {
            // The item's effects' output, after what it shows (ADR-0065 §5).
            _ if placed.effects.is_some() => {
                let owner = ChainOwner::Item(id.clone(), placed.item.clone());
                passes.filtered(shot, &owner, done);
                GpuKey::Filtered(owner)
            }
            SourceRef::Boxed(..) => continue,
            SourceRef::Frame(source) => GpuKey::Frame(source.clone()),
            SourceRef::Capture(spec) => GpuKey::Capture(spec.clone()),
            SourceRef::Filtered(source) => {
                let owner = ChainOwner::Source(source.clone());
                passes.filtered(shot, &owner, done);
                GpuKey::Filtered(owner)
            }
            SourceRef::Scene(inner) => {
                if depth >= MAX_NESTING {
                    tracing::debug!(
                        scene = %inner,
                        max = MAX_NESTING,
                        "nested scene is past the depth bound; not drawn"
                    );
                    continue;
                }
                if path.contains(inner) {
                    continue;
                }
                let owner = ChainOwner::Scene(inner.clone());
                if shot.chains.contains_key(&owner) {
                    passes.filtered(shot, &owner, done);
                    GpuKey::Filtered(owner)
                } else {
                    visit(shot, inner, depth + 1, path, done, passes)
                }
            }
        };
        layers.push(Layer {
            item: placed.item.clone(),
            image,
            placement: placed.placement,
            crop: placed.crop,
            opacity: placed.opacity,
            boxed: placed.effects,
        });
    }
    path.remove(id);
    passes.pass(Pass {
        scene: id.clone(),
        depth,
        key,
        layers,
    });
}

/// The video filters `scene` shows: its own, or its parent's for a keyframe scene,
/// which has none of its own (ADR-0024 §2).
fn scene_filters<'s>(state: &'s State, scene: &'s GraphScene) -> &'s [Filter] {
    scene
        .variant_of()
        .and_then(|parent| state.scene(parent))
        .unwrap_or(scene)
        .filters()
        .chain(FilterChain::Video)
}

/// Whether `item`, which places a scene, shows it exactly as the scene draws on the
/// canvas: over the whole canvas, unrotated and uncropped.
fn covers(item: &GraphItem, canvas: Canvas) -> bool {
    if *item.crop() != ProtocolCrop::NONE {
        return false;
    }
    match item.placement() {
        GraphPlacement::Geometry(geometry) => {
            ItemGeometry::covering_canvas(canvas.width, canvas.height).is_ok_and(|whole| {
                geometry.center == whole.center
                    && geometry.size == whole.size
                    && geometry.rotation_deg.get() == 0.0
                    && geometry.is_neutral_3d()
            })
        }
        GraphPlacement::Legacy(transform) => {
            *transform == jstream_protocol::ItemTransform::IDENTITY
        }
        _ => false,
    }
}

/// An item's box for a transition: its geometry, or the box its legacy transform draws
/// for an image of `size` (the canvas when unknown, as for the captured display). `None`
/// when the crop or the scale leaves nothing. A legacy negative scale's reflection is
/// not kept while the item moves.
fn endpoint_geometry(
    item: &GraphItem,
    size: Option<[u32; 2]>,
    canvas: Canvas,
) -> Option<ItemGeometry> {
    match item.placement() {
        GraphPlacement::Geometry(geometry) => Some(*geometry),
        GraphPlacement::Legacy(transform) => {
            let [width, height] = size.unwrap_or([canvas.width, canvas.height]);
            ItemGeometry::from_legacy(*transform, *item.crop(), width, height)
                .ok()
                .flatten()
                .map(|boxed| boxed.geometry)
        }
        _ => None,
    }
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

/// The live values (ADR-0032) the engine publishes on the render parameter channel:
/// each item's placement or crop as a gesture holds it now, overriding the one in the
/// [`Shot`] until the gesture ends. The render thread and the snapshot thread apply it as
/// they draw, so a live value needs no new snapshot. Built on the control side; the
/// render thread only looks items up in it, which allocates nothing.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct LiveParams {
    items: BTreeMap<ItemId, LiveItem>,
}

/// One item's live values: either may be absent, leaving the shot's.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct LiveItem {
    placement: Option<Placement>,
    crop: Option<Crop>,
}

impl LiveParams {
    /// The overrides `values` set, later ones winning. Each is an `item.set_transform`,
    /// `item.set_geometry` or `item.set_crop`; any other edit sets nothing.
    pub(crate) fn from_values<'a>(values: impl IntoIterator<Item = &'a Edit>) -> Self {
        let mut items: BTreeMap<ItemId, LiveItem> = BTreeMap::new();
        for value in values {
            match value {
                Edit::ItemSetTransform {
                    item_id, transform, ..
                } => {
                    items.entry(item_id.clone()).or_default().placement =
                        Some(placement(&GraphPlacement::Legacy(*transform)));
                }
                Edit::ItemSetGeometry {
                    item_id, geometry, ..
                } => {
                    items.entry(item_id.clone()).or_default().placement =
                        Some(placement(&GraphPlacement::Geometry(*geometry)));
                }
                Edit::ItemSetCrop {
                    item_id, crop: c, ..
                } => {
                    items.entry(item_id.clone()).or_default().crop = Some(crop(c));
                }
                _ => {}
            }
        }
        Self { items }
    }

    /// The live placement and crop of `item_id`, for tests.
    #[cfg(test)]
    pub(crate) fn item(&self, item_id: &ItemId) -> Option<(Option<Placement>, Option<Crop>)> {
        self.items
            .get(item_id)
            .map(|live| (live.placement, live.crop))
    }

    /// `placement` and `crop`, item `item`'s in the shot, with its live values applied.
    fn apply_to(&self, item: &ItemId, placement: Placement, crop: Crop) -> (Placement, Crop) {
        match self.items.get(item) {
            Some(live) => (
                live.placement.unwrap_or(placement),
                live.crop.unwrap_or(crop),
            ),
            None => (placement, crop),
        }
    }

    /// `layer`'s placement and crop, with this item's live values applied.
    fn apply(&self, layer: &Layer) -> (Placement, Crop) {
        match self.items.get(&layer.item) {
            Some(live) => (
                live.placement.unwrap_or(layer.placement),
                live.crop.unwrap_or(layer.crop),
            ),
            None => (layer.placement, layer.crop),
        }
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

/// One snapshot request: the published render snapshot, the `rev` it answers with,
/// and where to send the answer.
struct Job {
    snapshot: Arc<RenderSnapshot>,
    live: Arc<LiveParams>,
    rev: Rev,
    /// The moment to draw, on the media clock: where a running transition is.
    at: MediaTime,
    reply: ReplyTx,
}

/// The `jstream-snapshot` thread, started on the first `video.snapshot`. It owns the
/// compositor. Dropping this closes the queue and joins the thread.
#[derive(Default)]
pub(crate) struct SnapshotWorker {
    queue: Mutex<Option<(Sender<Job>, JoinHandle<()>)>>,
}

impl SnapshotWorker {
    /// Queues `snapshot` with the live values `live` applied, as it is at `at`, answering
    /// with `rev`. The returned future resolves when the thread has drawn it.
    pub(crate) fn request(
        &self,
        snapshot: Arc<RenderSnapshot>,
        live: Arc<LiveParams>,
        rev: Rev,
        at: MediaTime,
    ) -> SnapshotReply {
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
            && let Err(mpsc::SendError(job)) = tx.send(Job {
                snapshot,
                live,
                rev,
                at,
                reply,
            })
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

/// The snapshot compositor and the images it keeps between snapshots.
type Gpu = (Compositor, ResourceCache<GpuKey>, Chains);

/// Serves snapshot jobs in order until the queue closes. A panic while drawing is
/// caught per job: that job answers `internal` and the compositor is reopened next time.
fn worker(jobs: Receiver<Job>) {
    let mut gpu: Option<Gpu> = None;
    let mut backoff = OpenBackoff::default();
    for Job {
        snapshot,
        live,
        rev,
        at,
        reply,
    } in jobs
    {
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (compositor, cache, chains) = open(&mut gpu, &mut backoff)?;
            draw(snapshot.shot(), &live, rev, at, compositor, (cache, chains))
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
    gpu: &'a mut Option<Gpu>,
    backoff: &mut OpenBackoff,
) -> Result<&'a mut Gpu, ProtocolError> {
    if gpu.is_none() {
        let now = Instant::now();
        if let Some(error) = backoff.cached(now) {
            return Err(error);
        }
        match Compositor::open(OpenOptions::hardware()) {
            Ok(compositor) => {
                tracing::info!(
                    adapter = %compositor.adapter_description(),
                    "snapshot compositor opened"
                );
                backoff.opened();
                *gpu = Some((compositor, ResourceCache::new(), Chains::default()));
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

/// Renders `shot` with `live` applied at the canvas size and encodes the capped PNG.
/// The images and filters `cache` no longer needs are dropped here, on the snapshot
/// thread.
fn draw(
    shot: &Shot,
    live: &LiveParams,
    rev: Rev,
    at: MediaTime,
    compositor: &mut Compositor,
    (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
) -> Result<QueryResult, ProtocolError> {
    cache.begin_frame();
    let output = compose(
        compositor,
        (&mut *cache, &mut *chains),
        (shot, at),
        View::default(),
        live,
        &NoCaptures,
        DrawAt::read(shot.canvas().map_err(from_render)?),
    );
    drop(cache.end_frame(compositor));
    let full = output
        .map_err(from_render)?
        .pixels()
        .cloned()
        .ok_or_else(|| ProtocolError::Internal {
            reason: "reading the frame back produced no pixels".into(),
        })?;
    let (width, height) = fit(full.width(), full.height());
    let rgba = scale_down(full.width(), full.height(), full.rgba(), width, height);
    let png =
        encode_png(width, height, &rgba).map_err(|reason| ProtocolError::Internal { reason })?;
    Ok(QueryResult::VideoSnapshot {
        rev,
        width,
        height,
        png: base64(&png),
    })
}

impl Shot {
    /// The collection's canvas (ADR-0022), which every scene is composited at.
    pub(crate) fn canvas(&self) -> Result<FrameSize, RenderError> {
        FrameSize::new(self.canvas.width, self.canvas.height).map_err(|_| RenderError::Invalid {
            what: "canvas",
            detail: "the canvas size is zero".into(),
        })
    }
}

/// The latest frame of one of the pipeline's captures, which `display` and `window`
/// sources show on the render thread. `version` changes with every new frame, so an
/// unchanged CPU capture is not uploaded again.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CaptureFrame<'a> {
    pub(crate) version: u64,
    pub(crate) image: CaptureImage<'a>,
}

/// Where a [`CaptureFrame`]'s pixels are.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CaptureImage<'a> {
    /// Tightly packed straight sRGB RGBA8 at `size`, uploaded on a new version.
    Rgba { size: FrameSize, rgba: &'a [u8] },
    /// A texture shared with the platform, on the compositor's device: sampled where
    /// it is, with no CPU copy ([`ResourceCache::import`]).
    Gpu(&'a GpuFrame),
}

/// One capture the output pipeline runs: what it captures, for which source, and
/// whether the pointer shows in it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CaptureKey {
    /// What is captured.
    pub(crate) spec: CaptureSpec,
    /// The source it is captured for: `None` for the primary display that fills an
    /// output without a program scene, which no source owns.
    pub(crate) source: Option<SourceId>,
    /// The source's `show_cursor` setting. Off, the capture asks for the pointer to be
    /// left out.
    pub(crate) show_cursor: bool,
}

impl CaptureKey {
    /// The capture of `spec` for the source `source`, showing the pointer. A video
    /// device opens once, so every source showing it shares one capture: its key names
    /// no source.
    pub(crate) fn for_source(spec: CaptureSpec, source: SourceId) -> Self {
        let source = (!matches!(spec, CaptureSpec::Device(_))).then_some(source);
        Self {
            spec,
            source,
            show_cursor: true,
        }
    }

    /// This capture, with the pointer shown or not.
    #[must_use]
    pub(crate) fn with_cursor(mut self, show_cursor: bool) -> Self {
        self.show_cursor = show_cursor;
        self
    }

    /// The primary display, for no source, showing the pointer.
    pub(crate) fn primary() -> Self {
        Self {
            spec: CaptureSpec::Display(None),
            source: None,
            show_cursor: true,
        }
    }
}

/// The latest frame of each capture a shot places ([`Shot::captures`]).
pub(crate) trait Captures {
    /// The latest frame of `spec`'s capture; `None` before its first frame, or when it
    /// isn't running.
    fn frame(&self, spec: &CaptureKey) -> Option<CaptureFrame<'_>>;
}

/// No captures, for `video.snapshot` and tests.
pub(crate) struct NoCaptures;

impl Captures for NoCaptures {
    fn frame(&self, _: &CaptureKey) -> Option<CaptureFrame<'_>> {
        None
    }
}

/// What [`compose`] draws into: the output's size, and whether the output carries a CPU
/// readback ([`Output::pixels`]). Without, only its texture is drawn, for a GPU consumer
/// such as an encode target's conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DrawAt {
    pub(crate) size: FrameSize,
    pub(crate) readback: bool,
}

impl DrawAt {
    /// `size`, read back.
    pub(crate) fn read(size: FrameSize) -> Self {
        Self {
            size,
            readback: true,
        }
    }
}

/// Draws `view.mix` of `shot` (the program mix for `None`) as it is at `at` (a running
/// transition samples it) at `to`'s size, over opaque black, blurred when `view.privacy`
/// is `Some(true)` (ADR-0027 §4), and reads it back when `to` asks for that,
/// between the caller's [`ResourceCache::begin_frame`] and
/// [`ResourceCache::end_frame`]. Several mixes may be drawn between the two, so their
/// shared uploads and layers are kept for all of them.
/// An item with a live value in `live` is drawn with it instead of its placement or
/// crop in `shot`.
///
/// Every image comes from `cache`, and every source's video chain runs in `chains`
/// once per frame of the cache, however many mixes draw it (`run_chains`, through
/// [`Chains::begin`] and [`Chains::run_one`]), after the passes of the scenes they read: a source
/// frame is uploaded only when its version changed, and each nested scene is composited into the layer it had last frame
/// ([`Compositor::render_into`]), so a steady frame allocates no texture. Each nested
/// scene is premultiplied linear `Rgba16Float` that stays on the GPU, transparent where
/// it draws nothing. The mix's scene is drawn at the canvas size: straight to the output
/// when that is the canvas size, else into a layer that the output fits inside itself,
/// centred, with black bars where the aspect ratios differ (ADR-0022). Without a scene
/// to draw (no program scene) the primary display's capture is scaled to fill the
/// canvas, which is fitted the same way, or the output is black.
///
/// # Errors
/// What the compositor reports for an upload or a pass.
pub(crate) fn compose(
    compositor: &mut Compositor,
    (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
    (shot, at): (&Shot, MediaTime),
    view: View<'_>,
    live: &LiveParams,
    captures: &dyn Captures,
    to: DrawAt,
) -> Result<Output, RenderError> {
    let DrawAt {
        size: output,
        readback,
    } = to;
    let canvas = shot.canvas()?;
    for (key, source) in &shot.uploads {
        let Some(frame) = shot.frames.get(source) else {
            continue;
        };
        match &frame.pixels {
            // Sampled where it is, on the context it was painted on. Another compositor
            // (`video.snapshot`'s, or a program stage opened since) can't read it.
            Pixels::Gpu { frame, context } => {
                if compositor.context() == Some(context) {
                    cache.import_premultiplied(compositor, key, frame)?;
                }
            }
            Pixels::Copied(_) | Pixels::Video(_) => {
                cache.upload(
                    compositor,
                    key,
                    frame.version,
                    frame.width,
                    frame.height,
                    frame.rgba(),
                )?;
                // Every time: a kept upload may have been placed at another extent.
                if let Ok(extent) = FrameSize::new(frame.extent[0], frame.extent[1]) {
                    cache.set_extent(key, extent);
                }
            }
        }
    }
    let passes = shot.passes_of(view.mix);
    let tracks = shot
        .transition
        .as_ref()
        .filter(|_| shot.shows_program(view.mix));
    let mut fallback = None;
    for spec in &shot.captures {
        let Some(capture) = captures.frame(spec) else {
            continue;
        };
        let key = GpuKey::Capture(spec.clone());
        match capture.image {
            CaptureImage::Rgba { size, rgba } => cache.upload(
                compositor,
                &key,
                capture.version,
                size.width(),
                size.height(),
                rgba,
            )?,
            // Opaque, as the CPU path ignores a capture's alpha (#451).
            CaptureImage::Gpu(frame) => cache.import_opaque(compositor, &key, frame)?,
        };
        fallback = Some(key);
    }
    run_chains(compositor, (&mut *cache, &mut *chains), shot, live, at)?;
    if view.privacy.is_some() {
        privacy_layers(compositor, cache, output)?;
    }
    let (root, nested) = match (tracks, passes.split_last()) {
        (Some(_), _) => (None, passes),
        (None, Some((root, nested))) => (Some(root), nested),
        (None, None) => {
            let image = fallback.as_ref().and_then(|key| cache.get(key));
            // Scaled to fill the canvas, which the output follows (not the display), and
            // fitted inside the output like the canvas layer.
            let (scale, offset) = fit_inside(canvas, output);
            let items: Vec<Item<'_>> = image
                .into_iter()
                .map(|image| {
                    let sx = canvas.width() as f32 * scale / image.width() as f32;
                    let sy = canvas.height() as f32 * scale / image.height() as f32;
                    Item {
                        image,
                        placement: Placement::Legacy(LegacyTransform {
                            position: offset,
                            scale: [sx, sy],
                            ..LegacyTransform::IDENTITY
                        }),
                        crop: Crop::NONE,
                        opacity: OPAQUE,
                        visible: true,
                    }
                })
                .collect();
            let shown = scene(output, Color::BLACK, &items);
            return present(compositor, cache, &shown, view.privacy, readback);
        }
    };
    let direct = output == canvas;
    claim_layers(
        compositor,
        (&mut *cache, &mut *chains),
        shot,
        view.mix,
        output,
    )?;
    let cache = &*cache;
    let chains = &*chains;
    let image = |key: &GpuKey| {
        cache.get(key).ok_or_else(|| RenderError::Invalid {
            what: "cache",
            detail: format!("no image for {key:?}"),
        })
    };
    let mut items: Vec<Item<'_>> = Vec::new();
    for pass in nested {
        fill_items(&mut items, (cache, chains), pass, live);
        compositor.render_into(
            &scene(canvas, Color::TRANSPARENT, &items),
            image(&pass.key)?,
        )?;
    }
    match (root, tracks) {
        (_, Some(transition)) => {
            fill_tracks(&mut items, (cache, chains), transition, canvas, at);
        }
        (Some(root), None) => fill_items(&mut items, (cache, chains), root, live),
        (None, None) => items.clear(),
    }
    if direct {
        let shown = scene(output, Color::BLACK, &items);
        return present(compositor, cache, &shown, view.privacy, readback);
    }
    let layer = image(&GpuKey::Program)?;
    compositor.render_into(&scene(canvas, Color::BLACK, &items), layer)?;
    let placed = [fitted(layer, canvas, output)];
    let shown = scene(output, Color::BLACK, &placed);
    present(compositor, cache, &shown, view.privacy, readback)
}

/// Draws `shown`, the finished picture at the output size: as it is, or with the
/// privacy screen on, blurred by halving it step by step into the [`GpuKey::Privacy`]
/// layers and scaling the smallest back up, and reads it back when `readback` is set. An
/// error here fails the frame, and the
/// render thread then sends black, never the picture (ADR-0027 §4: it fails closed).
fn present(
    compositor: &mut Compositor,
    cache: &ResourceCache<GpuKey>,
    shown: &Scene<'_>,
    privacy: Option<bool>,
    readback: bool,
) -> Result<Output, RenderError> {
    if privacy != Some(true) {
        return compositor.render(shown, readback);
    }
    let layer = |size: FrameSize| {
        cache
            .get(&GpuKey::Privacy(size.width(), size.height()))
            .ok_or_else(|| RenderError::Invalid {
                what: "cache",
                detail: format!("no privacy layer at {size:?}"),
            })
    };
    let output = shown.canvas;
    compositor.render_into(shown, layer(output)?)?;
    let mut from = output;
    for to in privacy_steps(output) {
        let placed = [stretched(layer(from)?, from, to)];
        compositor.render_into(&scene(to, Color::BLACK, &placed), layer(to)?)?;
        from = to;
    }
    let placed = [stretched(layer(from)?, from, output)];
    compositor.render(&scene(output, Color::BLACK, &placed), readback)
}

/// Runs `shot`'s video chains, once per frame of the cache however many mixes draw it
/// ([`Chains::begin`]), each after the passes of the scenes it reads, which are drawn
/// into their [`GpuKey::reference`] layers first ([`schedule_chains`]).
///
/// # Errors
/// What the compositor reports for a scene layer or pass. That fails the frame, as a
/// failed nested pass does: the chains after it don't run, and since [`Chains::begin`]
/// has marked the frame, no later mix of the tick runs them either.
fn run_chains(
    compositor: &mut Compositor,
    (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
    shot: &Shot,
    live: &LiveParams,
    at: MediaTime,
) -> Result<(), RenderError> {
    if !chains.begin(compositor, cache, &shot.chains) {
        return Ok(());
    }
    let canvas = shot.canvas()?;
    for step in &shot.chain_steps {
        match step {
            ChainStep::Run(source) => {
                if let Some(chain) = shot.chains.get(source) {
                    chains.run_one(compositor, cache, (source, chain), at);
                }
            }
            ChainStep::Box(owner) => {
                if let Some(boxed) = shot.boxes.get(owner) {
                    draw_box(
                        compositor,
                        (&mut *cache, &mut *chains),
                        shot,
                        owner,
                        boxed,
                        live,
                    )?;
                }
            }
            ChainStep::Pass(pass) => {
                cache.layer(compositor, &pass.key, canvas.width(), canvas.height())?;
                let mut items = Vec::new();
                fill_items(&mut items, (&*cache, &*chains), pass, live);
                let Some(layer) = cache.get(&pass.key) else {
                    continue;
                };
                compositor.render_into(&scene(canvas, Color::TRANSPARENT, &items), layer)?;
            }
        }
    }
    Ok(())
}

/// The image an item's effects read: what the item shows, as the chain phase draws it.
/// A nested scene is drawn there at [`REFERENCE_DEPTH`], or read from its own chain.
fn box_input(shot: &Shot, source: &SourceRef) -> Option<GpuKey> {
    Some(match source {
        SourceRef::Scene(inner) if !shot.chains.contains_key(&ChainOwner::Scene(inner.clone())) => {
            GpuKey::reference(inner)
        }
        SourceRef::Boxed(..) => return None,
        other => key(other, shot),
    })
}

/// Draws the item of `boxed`, the effects of `owner`, into its [`GpuKey::ItemBox`]
/// layer before they run: cropped and fitted into its box, centred, flat and
/// unrotated, with its live values applied (ADR-0065 §5). An item whose image isn't
/// there this frame (a failed upload or chain) fails its chain, so it draws nothing.
///
/// # Errors
/// What the compositor reports for the layer or its pass.
fn draw_box(
    compositor: &mut Compositor,
    (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
    shot: &Shot,
    owner: &ChainOwner,
    boxed: &ItemBox,
    live: &LiveParams,
) -> Result<(), RenderError> {
    let ChainOwner::Item(scene_id, item_id) = owner else {
        return Ok(());
    };
    let key = GpuKey::ItemBox(scene_id.clone(), item_id.clone());
    let [width, height] = boxed.boxed.layer;
    cache.layer(compositor, &key, width, height)?;
    let input = box_input(shot, &boxed.source).filter(|input| !chains.failed(input));
    let (placement, crop) = live.apply_to(&boxed.item, boxed.placement, boxed.crop);
    let inside = boxed
        .boxed
        .geometry_of(placement, crop)
        .and_then(|geometry| boxed.boxed.inside(&geometry));
    let (Some(image), Some(inside), Some(layer)) = (
        input.as_ref().and_then(|input| cache.get(input)),
        inside,
        cache.get(&key),
    ) else {
        chains.fail(owner);
        return Ok(());
    };
    let items = [Item {
        image,
        placement: Placement::Geometry(inside),
        crop,
        opacity: OPAQUE,
        visible: true,
    }];
    let size = FrameSize::new(width, height).map_err(|_| RenderError::Invalid {
        what: "item box",
        detail: format!("an effects layer of {width}x{height}"),
    })?;
    compositor.render_into(&scene(size, Color::TRANSPARENT, &items), layer)
}

/// Keeps the privacy blur's layers for an output of `output`'s size in the cache, so
/// they are there, and allocated, before the screen is turned on.
pub(crate) fn privacy_layers(
    compositor: &Compositor,
    cache: &mut ResourceCache<GpuKey>,
    output: FrameSize,
) -> Result<(), RenderError> {
    for at in std::iter::once(output).chain(privacy_steps(output)) {
        cache.layer(
            compositor,
            &GpuKey::Privacy(at.width(), at.height()),
            at.width(),
            at.height(),
        )?;
    }
    Ok(())
}

/// Marks the layers [`compose`] draws `mix` at `output` through as used in the cache's
/// current frame, allocating any that are missing, without drawing: each nested
/// scene's, the program layer when `output` isn't the canvas size, and the layers of the
/// video chains that ran in an earlier frame ([`Chains::claim`]). The render
/// thread calls it for every mix not due on a tick, so a mix with a divisor keeps its
/// layers between the ticks it draws on and a steady frame allocates no texture.
///
/// # Errors
/// What the compositor reports for a layer it allocates.
pub(crate) fn claim_layers(
    compositor: &mut Compositor,
    (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
    shot: &Shot,
    mix: Option<&VideoMixId>,
    output: FrameSize,
) -> Result<(), RenderError> {
    chains.claim(compositor, cache);
    let canvas = shot.canvas()?;
    for step in &shot.chain_steps {
        match step {
            ChainStep::Pass(pass) => {
                cache.layer(compositor, &pass.key, canvas.width(), canvas.height())?;
            }
            ChainStep::Box(owner) => {
                if let (ChainOwner::Item(scene, item), Some(boxed)) = (owner, shot.boxes.get(owner))
                {
                    let [width, height] = boxed.boxed.layer;
                    cache.layer(
                        compositor,
                        &GpuKey::ItemBox(scene.clone(), item.clone()),
                        width,
                        height,
                    )?;
                }
            }
            ChainStep::Run(_) => {}
        }
    }
    let passes = shot.passes_of(mix);
    let nested = match passes.split_last() {
        // A running transition draws its tracks over every pass, all of them nested.
        _ if shot.transition.is_some() && shot.shows_program(mix) => passes,
        Some((_, nested)) => nested,
        None => return Ok(()),
    };
    for pass in nested {
        cache.layer(compositor, &pass.key, canvas.width(), canvas.height())?;
    }
    if output != canvas {
        cache.layer(
            compositor,
            &GpuKey::Program,
            canvas.width(),
            canvas.height(),
        )?;
    }
    Ok(())
}

/// The sizes the privacy blur halves the output through after the output itself: at
/// least one step, then on until one is at most [`PRIVACY_SIDE`] on its longest side.
fn privacy_steps(output: FrameSize) -> impl Iterator<Item = FrameSize> {
    std::iter::successors(half(output), |size| {
        if size.width().max(size.height()) > PRIVACY_SIDE {
            half(*size)
        } else {
            None
        }
    })
}

/// Half of `size`, rounded up; `None` for a single pixel.
fn half(size: FrameSize) -> Option<FrameSize> {
    let half = FrameSize::new(size.width().div_ceil(2), size.height().div_ceil(2)).ok()?;
    (half != size).then_some(half)
}

/// `image`, of size `from`, scaled to cover `into` exactly, each side on its own.
fn stretched(image: &jstream_render::Image, from: FrameSize, into: FrameSize) -> Item<'_> {
    Item {
        image,
        placement: Placement::Legacy(LegacyTransform {
            scale: [
                into.width() as f32 / from.width() as f32,
                into.height() as f32 / from.height() as f32,
            ],
            ..LegacyTransform::IDENTITY
        }),
        crop: Crop::NONE,
        opacity: OPAQUE,
        visible: true,
    }
}

/// Replaces `items` with the layers of `pass`, with the live values in `live` applied. A
/// layer whose image is not cached (a failed or skipped upload), or whose video chain
/// failed this frame, draws nothing.
fn fill_items<'a>(
    items: &mut Vec<Item<'a>>,
    (cache, chains): (&'a ResourceCache<GpuKey>, &Chains),
    pass: &Pass,
    live: &LiveParams,
) {
    items.clear();
    items.extend(pass.layers.iter().filter_map(|layer| {
        if chains.failed(&layer.image) {
            return None;
        }
        let (placement, crop) = live.apply(layer);
        let (placement, crop) = match &layer.boxed {
            Some(boxed) => {
                let geometry = boxed.geometry_of(placement, crop)?;
                (Placement::Geometry(boxed.around(&geometry)?), Crop::NONE)
            }
            None => (placement, crop),
        };
        Some(Item {
            image: cache.get(&layer.image)?,
            placement,
            crop,
            opacity: layer.opacity,
            visible: true,
        })
    }));
}

/// Replaces `items` with `transition`'s tracks sampled at `at`, in drawing order. A
/// track that draws nothing then, or whose image is not cached, is left out.
fn fill_tracks<'a>(
    items: &mut Vec<Item<'a>>,
    (cache, chains): (&'a ResourceCache<GpuKey>, &Chains),
    transition: &Transitioning,
    canvas: FrameSize,
    at: MediaTime,
) {
    let elapsed = transition::elapsed_ms(transition.start, at);
    let size = [canvas.width() as f32, canvas.height() as f32];
    items.clear();
    items.extend(transition.tracks.iter().filter_map(|(key, track)| {
        if chains.failed(key) {
            return None;
        }
        let sampled = track.sample(elapsed, size)?;
        Some(Item {
            image: cache.get(key)?,
            placement: Placement::Geometry(sampled.geometry),
            crop: sampled.crop,
            opacity: sampled.opacity,
            visible: true,
        })
    }));
}

/// `image`, of size `from`, fitted inside `into`: scaled by the same factor both ways
/// and centred, so it is never stretched and the rest stays as it was (ADR-0022).
fn fitted(image: &jstream_render::Image, from: FrameSize, into: FrameSize) -> Item<'_> {
    let (scale, offset) = fit_inside(from, into);
    Item {
        image,
        placement: Placement::Legacy(LegacyTransform {
            position: offset,
            scale: [scale, scale],
            ..LegacyTransform::IDENTITY
        }),
        crop: Crop::NONE,
        opacity: OPAQUE,
        visible: true,
    }
}

/// The scale and top-left offset that fit `from` inside `into`, centred.
fn fit_inside(from: FrameSize, into: FrameSize) -> (f32, [f32; 2]) {
    let (fw, fh) = (from.width() as f32, from.height() as f32);
    let (iw, ih) = (into.width() as f32, into.height() as f32);
    let scale = (iw / fw).min(ih / fh);
    (scale, [(iw - fw * scale) / 2.0, (ih - fh * scale) / 2.0])
}

fn scene<'a>(canvas: FrameSize, background: Color, items: &'a [Item<'a>]) -> Scene<'a> {
    Scene {
        canvas,
        background,
        items,
    }
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
    let len = reader
        .output_buffer_size()
        .ok_or("frame size overflows usize")?;
    let mut buf = vec![0u8; len];
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
pub(crate) mod tests {
    use super::{
        CaptureFrame, CaptureImage, Captures, Chain, ChainOwner, Chains, FIRST_RETRY, Frame,
        GpuKey, LiveParams, MAX_NESTING, MAX_RETRY, OpenBackoff, Placed, Shot, SourceFrameError,
        SourceRef, View, base64, captures, claim_layers, compose, decode_base64, decode_png, draw,
        encode_png, fit, is_no_adapter, schedule,
    };
    use jstream_media::{FrameSize, MediaTime, Timebase};
    use jstream_protocol::{
        Canvas, Edit, ItemId, ProtocolError, QueryResult, Rev, SceneId, SourceId,
    };
    use jstream_render::{
        Compositor, Crop, LegacyTransform, OpenOptions, Placement, ResourceCache,
    };
    use jstream_sources::CaptureSpec;
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    use super::CaptureKey;
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
        Frame::new(16, 9, &bytes, u64::from(rgba[1])).expect("invariant: 16x9 RGBA8")
    }

    /// An item's effects layer is its box plus the outset on every side (ADR-0065 §5):
    /// the box is drawn centred in it, flat, and the layer is placed over the box grown
    /// in proportion, turned as the box is. A box too large for one texture is scaled
    /// down uniformly, and a legacy transform has the box its quad covers.
    #[test]
    fn an_effects_layer_is_the_box_plus_its_outset() {
        use jstream_protocol::{Finite32, FitMode, ItemGeometry, Size, Vec2};
        let mut placed = ItemGeometry::try_new(
            Vec2::new(500.0, 300.0).expect("finite"),
            Size::try_new(100.0, 50.0).expect("positive"),
        )
        .expect("valid");
        placed.rotation_deg = Finite32::new(30.0).expect("finite");
        placed.flip_h = true;
        placed.fit_mode = FitMode::Fill;
        let boxed = super::Boxed::new(&placed, 4, None).expect("finite");
        assert_eq!(boxed.layer, [108, 58]);

        let inside = boxed.inside(&placed).expect("inside");
        assert_eq!((inside.center.x.get(), inside.center.y.get()), (54.0, 29.0));
        assert_eq!(
            (inside.size.width.get(), inside.size.height.get()),
            (100.0, 50.0)
        );
        assert_eq!(inside.rotation_deg.get(), 0.0);
        assert!(
            inside.flip_h && inside.fit_mode == FitMode::Fill,
            "the item's own"
        );

        // A live box twice as wide grows its outset with it.
        let mut wide = placed;
        wide.size = Size::try_new(200.0, 50.0).expect("positive");
        let around = boxed.around(&wide).expect("around");
        assert_eq!(
            (around.size.width.get(), around.size.height.get()),
            (216.0, 58.0)
        );
        assert_eq!(around.center, placed.center);
        assert_eq!(around.rotation_deg.get(), 30.0, "it turns with the item");
        assert!(!around.flip_h && around.fit_mode == FitMode::Stretch);

        let mut huge = placed;
        huge.size = Size::try_new(16_384.0, 100.0).expect("positive");
        let scaled = super::Boxed::new(&huge, 0, None).expect("finite");
        assert_eq!(scaled.layer, [8192, 50]);
        assert_eq!(scaled.scale, 0.5);

        let legacy = Placement::Legacy(LegacyTransform {
            position: [10.0, 20.0],
            scale: [2.0, 2.0],
            rotation_deg: 0.0,
        });
        let unknown = super::Boxed::new(&placed, 0, None).expect("finite");
        assert!(
            unknown.geometry_of(legacy, Crop::NONE).is_none(),
            "no size known"
        );
        let known = super::Boxed::new(&placed, 0, Some([16, 9])).expect("finite");
        let of = known.geometry_of(legacy, Crop::NONE).expect("its box");
        assert_eq!((of.size.width.get(), of.size.height.get()), (32.0, 18.0));
        assert_eq!((of.center.x.get(), of.center.y.get()), (26.0, 29.0));
    }

    fn placed(source: SourceRef, placement: Placement) -> Placed {
        Placed {
            item: ItemId::new("itm"),
            source,
            placement,
            crop: Crop::NONE,
            opacity: super::OPAQUE,
            effects: None,
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
        let mut shot = Shot {
            canvas: Canvas::DEFAULT,
            program: program.clone(),
            scenes: scenes
                .into_iter()
                .map(|(id, items)| (SceneId::new(id), items))
                .collect(),
            frames: BTreeMap::from([
                (SourceId::new("red"), frame(RED)),
                (SourceId::new("green"), frame(GREEN)),
            ]),
            ..Shot::default()
        };
        if let Some(program) = &program {
            shot.passes = schedule(&shot, program);
        }
        shot.uploads = shot
            .frames
            .keys()
            .map(|id| (GpuKey::Frame(id.clone()), id.clone()))
            .collect();
        shot.captures = captures(&shot.passes);
        shot
    }

    /// What the primary display shows in these tests.
    fn primary() -> SourceRef {
        SourceRef::Capture(CaptureKey::primary())
    }

    /// Every capture shows `frame`.
    struct Every<'a>(Option<CaptureFrame<'a>>);

    impl Captures for Every<'_> {
        fn frame(&self, _: &CaptureKey) -> Option<CaptureFrame<'_>> {
            self.0
        }
    }

    /// One program scene filled with red: a shot that differs from an empty one.
    pub(crate) fn one_scene_shot() -> Shot {
        shot(vec![("program", vec![fill("red")])])
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
        shot.passes
            .iter()
            .map(|pass| (pass.scene.as_str().to_owned(), pass.depth))
            .collect()
    }

    fn layer(scene: &str, depth: usize) -> GpuKey {
        GpuKey::Layer(SceneId::new(scene), depth)
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
        let sources: Vec<&GpuKey> = shot.passes[3].layers.iter().map(|l| &l.image).collect();
        assert_eq!(sources, [&layer("a", 1), &layer("b", 1), &layer("a", 1)]);
        assert_eq!(shot.passes[1].key, layer("a", 1));
        assert_eq!(
            shot.passes[1].layers[1].image,
            layer("b", 2),
            "`a` places the `b` pass one level down"
        );
    }

    /// A `scene` mix draws its own scene; program and preview mixes, no mix, and a mix
    /// the collection doesn't have draw the program's passes.
    #[test]
    fn a_scene_mix_plans_its_own_scene() {
        use jstream_graph::Graph;
        use jstream_protocol::{Edit, MixInput, MixResolution, VideoMix, VideoMixId};

        let mix = |id: &str, input| Edit::VideoMixCreate {
            mix_id: VideoMixId::new(id),
            name: id.into(),
            mix: VideoMix {
                input,
                resolution: MixResolution {
                    width: 640,
                    height: 360,
                },
                fps_divisor: 1,
            },
        };
        let mut graph = Graph::default();
        graph
            .apply(
                &[
                    Edit::SceneCreate {
                        scene_id: SceneId::new("main"),
                        name: "Main".into(),
                        role: None,
                    },
                    Edit::SceneCreate {
                        scene_id: SceneId::new("cam"),
                        name: "Camera".into(),
                        role: None,
                    },
                    mix(
                        "side",
                        MixInput::Scene {
                            scene_id: SceneId::new("cam"),
                        },
                    ),
                    mix("prog", MixInput::Program),
                    mix("prev", MixInput::Preview),
                ],
                None,
            )
            .expect("edits");
        let shot = super::plan(&graph, &BTreeMap::new());
        let root = |id: Option<&str>| {
            let id = id.map(VideoMixId::new);
            shot.root_of(id.as_ref())
                .map(|scene| scene.as_str().to_owned())
        };
        assert_eq!(root(Some("side")).as_deref(), Some("cam"));
        for mix in [None, Some("prog"), Some("prev"), Some("gone")] {
            assert_eq!(root(mix).as_deref(), Some("main"), "{mix:?}");
        }
    }

    /// A display or window that only a `scene` mix shows is among the shot's captures,
    /// beside the program's, so the render thread starts it for that mix.
    #[test]
    fn a_capture_only_a_mix_shows_is_captured() {
        use jstream_graph::Graph;
        use jstream_protocol::{
            Edit, ItemId, MixInput, MixResolution, SourceKind, VideoMix, VideoMixId,
        };

        let display = |id: &str| Edit::SourceCreate {
            source_id: SourceId::new(id),
            name: id.into(),
            kind: SourceKind::Builtin {
                kind: "display".into(),
            },
            settings: Default::default(),
            duplicated_from: None,
        };
        let add = |scene: &str, source: &str| Edit::ItemAdd {
            scene_id: SceneId::new(scene),
            item_id: ItemId::new(source),
            source_id: SourceId::new(source),
            transform: None,
            geometry: None,
            opacity: None,
            blend: None,
        };
        let mut graph = Graph::new().with_kinds(crate::kinds::builtin());
        graph
            .apply(
                &[
                    Edit::SceneCreate {
                        scene_id: SceneId::new("main"),
                        name: "Main".into(),
                        role: None,
                    },
                    Edit::SceneCreate {
                        scene_id: SceneId::new("cam"),
                        name: "Camera".into(),
                        role: None,
                    },
                    display("primary"),
                    display("second"),
                    add("main", "primary"),
                    add("cam", "second"),
                    Edit::VideoMixCreate {
                        mix_id: VideoMixId::new("side"),
                        name: "Side".into(),
                        mix: VideoMix {
                            input: MixInput::Scene {
                                scene_id: SceneId::new("cam"),
                            },
                            resolution: MixResolution {
                                width: 640,
                                height: 360,
                            },
                            fps_divisor: 1,
                        },
                    },
                ],
                None,
            )
            .expect("edits");
        let second = CaptureSpec::Display(Some("monitor:2".into()));
        let shot = super::plan_with(
            &graph,
            Some(&SceneId::new("main")),
            None,
            &mut |id| {
                let spec = match id.as_str() {
                    "second" => second.clone(),
                    _ => CaptureSpec::Display(None),
                };
                Some(super::Look::Capture(CaptureKey::for_source(
                    spec,
                    id.clone(),
                )))
            },
            &crate::filter_assets::FilterAssets::default(),
        )
        .shot;
        let specs: BTreeSet<_> = shot.captures().iter().map(|k| k.spec.clone()).collect();
        assert_eq!(specs, BTreeSet::from([CaptureSpec::Display(None), second]));
        assert!(
            shot.captures().iter().all(|k| k.source.is_some()),
            "each capture is for its source"
        );
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
        assert!(
            shot.passes[0].layers.is_empty(),
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
        assert_eq!(shot.passes[0].layers.len(), 1);
    }

    /// Draws `shot` on whatever adapter opens (software in CI) and decodes the 1280x720 PNG. `None` when there
    /// is no adapter outside CI.
    fn snap(shot: &Shot) -> Option<Vec<u8>> {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let mut compositor = match Compositor::open(OpenOptions::hardware()) {
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
        let mut cache = ResourceCache::new();
        let result = draw(
            shot,
            &LiveParams::default(),
            Rev(1),
            zero(),
            &mut compositor,
            (&mut cache, &mut Chains::default()),
        )
        .expect("draw");
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

    /// An item's opacity multiplies its alpha, and the result blends in linear light: half
    /// green over red is 0.5 of each in linear light, sRGB 188. A nested scene's opacity
    /// applies to the scene as a whole, and an item at 0 draws nothing.
    #[test]
    fn opacity_blends_items_and_nested_scenes_in_linear_light() {
        let faded = |placed: Placed, opacity: f32| Placed { opacity, ..placed };
        // The 16x9 frames are scaled to the canvas; a nested scene's layer is canvas-sized.
        let left = Placement::Legacy(LegacyTransform {
            scale: [60.0, 120.0],
            ..LegacyTransform::IDENTITY
        });
        let right = Placement::Legacy(LegacyTransform {
            position: [960.0, 0.0],
            scale: [0.5, 1.0],
            rotation_deg: 0.0,
        });
        let shot = shot(vec![
            (
                "program",
                vec![
                    fill("red"),
                    faded(placed(SourceRef::Frame(SourceId::new("green")), left), 0.5),
                    faded(nest("a", right), 0.5),
                    faded(fill("green"), 0.0),
                ],
            ),
            ("a", vec![fill("green")]),
        ]);
        let Some(rgba) = snap(&shot) else { return };
        let half = [188, 188, 0, 255];
        for (x, y) in [(100, 360), (1100, 360)] {
            let pixel = at(&rgba, x, y);
            for (got, want) in pixel.iter().zip(half) {
                assert!(got.abs_diff(want) <= 1, "({x}, {y}): {pixel:?}");
            }
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

    /// Opens whatever adapter opens (software in CI) for a test that drives [`compose`]
    /// itself. `None` when
    /// there is no adapter outside CI.
    pub(crate) fn gpu() -> Option<Compositor> {
        match Compositor::open(OpenOptions::hardware()) {
            Ok(compositor) => Some(compositor),
            Err(err) => {
                let error = super::from_render(err);
                assert!(is_no_adapter(&error), "opening the compositor: {error}");
                assert!(
                    std::env::var_os("CI").is_none(),
                    "CI has no wgpu adapter: {error}"
                );
                None
            }
        }
    }

    fn zero() -> MediaTime {
        MediaTime::zero(Timebase::new(1, 1000).expect("valid"))
    }

    /// IEC 61966-2-1 encode, as the output applies it.
    fn linear_to_srgb(c: f32) -> f32 {
        if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
    }

    /// One frame of `shot` at `size`, read back, as the render thread draws it.
    fn frame_at(
        compositor: &mut Compositor,
        cache: &mut ResourceCache<GpuKey>,
        shot: &Shot,
        capture: Option<&CaptureFrame<'_>>,
        size: FrameSize,
    ) -> jstream_render::Snapshot {
        live_frame_at(
            compositor,
            cache,
            shot,
            &LiveParams::default(),
            capture,
            size,
        )
    }

    /// [`frame_at`] with the live values `live` applied.
    fn live_frame_at(
        compositor: &mut Compositor,
        cache: &mut ResourceCache<GpuKey>,
        shot: &Shot,
        live: &LiveParams,
        capture: Option<&CaptureFrame<'_>>,
        size: FrameSize,
    ) -> jstream_render::Snapshot {
        cache.begin_frame();
        let captures = Every(capture.copied());
        let output = compose(
            compositor,
            (cache, &mut Chains::default()),
            (shot, zero()),
            View::default(),
            live,
            &captures,
            super::DrawAt::read(size),
        )
        .expect("compose");
        drop(cache.end_frame(compositor));
        output.pixels().expect("invariant: read back").clone()
    }

    /// A live value (ADR-0032) moves or crops its item where it is drawn, in every pass
    /// that places it, without a new shot; other items keep their placement.
    #[test]
    fn live_values_override_the_shot_as_it_is_drawn() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let size = FrameSize::new(32, 18).expect("invariant: non-zero");
        let top = Placed {
            item: ItemId::new("top"),
            ..fill("green")
        };
        let mix = shot(vec![("program", vec![fill("red"), top])]);
        let out = frame_at(&mut compositor, &mut cache, &mix, None, size);
        assert_eq!(out.pixel(24, 9), Some(GREEN), "the shot's placement");

        // The same scale over half the width: the red item below shows on the right.
        let half = Edit::ItemSetTransform {
            item_id: ItemId::new("top"),
            scene_id: None,
            transform: jstream_protocol::ItemTransform {
                scale: jstream_protocol::Vec2::new(60.0, 120.0).expect("finite"),
                ..jstream_protocol::ItemTransform::IDENTITY
            },
        };
        let live = LiveParams::from_values([&half]);
        let out = live_frame_at(&mut compositor, &mut cache, &mix, &live, None, size);
        assert_eq!(out.pixel(8, 9), Some(GREEN), "still on the left");
        assert_eq!(out.pixel(24, 9), Some(RED), "moved by its live value");

        // A live crop leaves the placement as the shot has it.
        let cropped = Edit::ItemSetCrop {
            item_id: ItemId::new("top"),
            scene_id: None,
            crop: jstream_protocol::Crop {
                right: 8,
                ..jstream_protocol::Crop::NONE
            },
        };
        let live = LiveParams::from_values([&cropped]);
        let out = live_frame_at(&mut compositor, &mut cache, &mix, &live, None, size);
        assert_eq!(out.pixel(8, 9), Some(GREEN));
        assert_eq!(out.pixel(24, 9), Some(RED), "cropped away on the right");
    }

    /// The render thread's output is the encoder size, not the canvas: with no program
    /// scene the capture fills it, a program scene is scaled from the canvas to fit it,
    /// and a `display` source shows the capture where its item is placed.
    #[test]
    fn the_program_fills_an_output_of_another_size() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let size = FrameSize::new(32, 18).expect("invariant: non-zero");
        let blue: Vec<u8> = std::iter::repeat_n([0, 0, 255, 255], 32 * 18)
            .flatten()
            .collect();
        let capture = CaptureFrame {
            version: 1,
            image: CaptureImage::Rgba { size, rgba: &blue },
        };

        let empty = Shot::default();
        let out = frame_at(&mut compositor, &mut cache, &empty, Some(&capture), size);
        assert_eq!(
            out.pixel(16, 9),
            Some([0, 0, 255, 255]),
            "the capture fills it"
        );
        let out = frame_at(&mut compositor, &mut cache, &empty, None, size);
        assert_eq!(
            out.pixel(16, 9),
            Some([0, 0, 0, 255]),
            "black with no capture"
        );

        let green = shot(vec![("program", vec![fill("green")])]);
        let out = frame_at(&mut compositor, &mut cache, &green, Some(&capture), size);
        for (x, y) in [(0, 0), (16, 9), (31, 17)] {
            assert_eq!(out.pixel(x, y), Some(GREEN), "({x}, {y})");
        }

        // The capture (32x18) scaled by 30 covers the canvas's left half at 0.5.
        let half = Placement::Legacy(LegacyTransform {
            scale: [30.0, 60.0],
            ..LegacyTransform::IDENTITY
        });
        let display = shot(vec![(
            "program",
            vec![fill("red"), placed(primary(), half)],
        )]);
        assert_eq!(display.captures, BTreeSet::from([CaptureKey::primary()]));
        let out = frame_at(&mut compositor, &mut cache, &display, Some(&capture), size);
        assert_eq!(
            out.pixel(4, 9),
            Some([0, 0, 255, 255]),
            "the capture on the left"
        );
        assert_eq!(out.pixel(28, 9), Some(RED), "red on the right");
    }

    /// A 32x18 red frame its source drew at a density of 2, so it stands for 16x9.
    fn dense_red() -> Frame {
        let bytes: Vec<u8> = std::iter::repeat_n(RED, 32 * 18).flatten().collect();
        Frame::new(32, 18, &bytes, 7)
            .expect("invariant: 32x18 RGBA8")
            .placed_at(FrameSize::new(16, 9).expect("invariant: non-zero"))
    }

    /// A frame drawn at a density is placed at its extent, not its pixels (ADR-0073).
    #[test]
    fn a_dense_frame_is_placed_at_its_extent() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let size = FrameSize::new(32, 18).expect("invariant: non-zero");
        // Scaled by 60, 16x9 is the canvas's top left quarter; its 32x18 pixels would be
        // all of it.
        let quarter = Placement::Legacy(LegacyTransform {
            scale: [60.0, 60.0],
            ..LegacyTransform::IDENTITY
        });
        let mut dense = shot(vec![(
            "program",
            vec![placed(SourceRef::Frame(SourceId::new("red")), quarter)],
        )]);
        dense.frames.insert(SourceId::new("red"), dense_red());
        let out = frame_at(&mut compositor, &mut cache, &dense, None, size);
        assert_eq!(out.pixel(4, 4), Some(RED), "inside the quarter");
        assert_ne!(out.pixel(28, 15), Some(RED), "outside it");
    }

    /// A shot measures how much each frame it draws is enlarged, against the frame's
    /// extent, the most of any of its items, and a chained source as 1 (ADR-0073).
    #[test]
    fn a_shot_measures_how_enlarged_its_frames_are() {
        use jstream_protocol::{ItemGeometry, Size};
        let boxed = Placement::Geometry(ItemGeometry {
            size: Size::try_new(64.0, 36.0).expect("positive"),
            ..ItemGeometry::covering_canvas(1920, 1080).expect("positive")
        });
        let half = Placement::Legacy(LegacyTransform {
            scale: [-0.5, 0.25],
            ..LegacyTransform::IDENTITY
        });
        let green = |placement| placed(SourceRef::Frame(SourceId::new("green")), placement);
        let mut planned = shot(vec![(
            "program",
            vec![fill("red"), green(boxed), green(half)],
        )]);
        let scales = |shot: &Shot| shot.scales().into_iter().collect::<Vec<_>>();
        assert_eq!(
            scales(&planned),
            [(SourceId::new("green"), 4.0), (SourceId::new("red"), 120.0)]
        );
        planned.frames.insert(SourceId::new("red"), dense_red());
        assert_eq!(scales(&planned)[1], (SourceId::new("red"), 120.0));
        planned.chains.insert(
            ChainOwner::Source(SourceId::new("red")),
            Chain {
                input: GpuKey::Frame(SourceId::new("red")),
                filters: Vec::new(),
            },
        );
        assert_eq!(scales(&planned)[1], (SourceId::new("red"), 1.0));
    }

    /// A mix whose aspect ratio differs from the canvas shows the canvas fitted inside
    /// it with black bars, never stretched (ADR-0022), and a `scene` mix draws its own
    /// scene, not the program.
    #[test]
    fn a_mix_letterboxes_the_canvas_and_draws_its_scene() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let square = FrameSize::new(32, 32).expect("invariant: non-zero");
        let mut shot = shot(vec![
            ("program", vec![fill("green")]),
            ("cam", vec![fill("red")]),
        ]);
        use jstream_protocol::VideoMixId;
        let side = VideoMixId::new("side");
        shot.mixes.insert(side.clone(), SceneId::new("cam"));
        shot.mix_passes
            .insert(SceneId::new("cam"), schedule(&shot, &SceneId::new("cam")));

        let mut draw = |mix: Option<&VideoMixId>| {
            cache.begin_frame();
            let output = compose(
                &mut compositor,
                (&mut cache, &mut Chains::default()),
                (&shot, zero()),
                View { mix, privacy: None },
                &LiveParams::default(),
                &super::NoCaptures,
                super::DrawAt::read(square),
            )
            .expect("compose");
            drop(cache.end_frame(&compositor));
            output.pixels().expect("invariant: read back").clone()
        };
        let program = draw(None);
        // 1920x1080 fitted into 32x32 is 32x18, centred: rows 7 to 24.
        assert_eq!(program.pixel(16, 16), Some(GREEN));
        for y in [0, 5, 27, 31] {
            assert_eq!(program.pixel(16, y), Some([0, 0, 0, 255]), "bar at row {y}");
        }
        let cam = draw(Some(&side));
        assert_eq!(cam.pixel(16, 16), Some(RED));
        assert_eq!(cam.pixel(16, 0), Some([0, 0, 0, 255]));
    }

    /// A GPU capture is sampled where it is: with no program scene it is scaled to fill
    /// an output of another size, and a `display` source shows it where its item is.
    #[test]
    fn a_gpu_capture_fills_the_output_and_shows_in_display_sources() {
        use jstream_media::{ColorInfo, MediaTime, PixelFormat, Timebase, VideoFrameInfo};
        use jstream_platform::{FrameSync, GpuFrame, PlatformError, wgpu};
        use std::sync::Arc;

        #[derive(Debug)]
        struct Ready;
        impl FrameSync for Ready {
            fn stage(&self, _queue: &wgpu::Queue) -> Result<(), PlatformError> {
                Ok(())
            }
        }

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let extent = wgpu::Extent3d {
            width: 16,
            height: 9,
            depth_or_array_layers: 1,
        };
        let texture = compositor
            .device()
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("jstream-core-test-capture"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
        // Blue in BGRA order.
        let blue: Vec<u8> = std::iter::repeat_n([255, 0, 0, 255], 16 * 9)
            .flatten()
            .collect();
        compositor.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &blue,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(16 * 4),
                rows_per_image: Some(9),
            },
            extent,
        );
        let frame = GpuFrame::new(
            texture,
            VideoFrameInfo {
                size: FrameSize::new(16, 9).expect("invariant: non-zero"),
                format: PixelFormat::Bgra8,
                color: ColorInfo::default(),
                pts: MediaTime::zero(Timebase::NANOS),
            },
            Arc::new(Ready),
        );
        let capture = CaptureFrame {
            version: 1,
            image: CaptureImage::Gpu(&frame),
        };
        let size = FrameSize::new(32, 18).expect("invariant: non-zero");

        let empty = Shot::default();
        let out = frame_at(&mut compositor, &mut cache, &empty, Some(&capture), size);
        for (x, y) in [(0, 0), (16, 9), (31, 17)] {
            assert_eq!(out.pixel(x, y), Some([0, 0, 255, 255]), "({x}, {y})");
        }
        assert_eq!(
            Arc::strong_count(frame.sync()),
            1,
            "the cache keeps no hold past the frame"
        );

        // The capture (16x9) scaled by 60 covers the canvas's left half.
        let half = Placement::Legacy(LegacyTransform {
            scale: [60.0, 120.0],
            ..LegacyTransform::IDENTITY
        });
        let display = shot(vec![(
            "program",
            vec![fill("red"), placed(primary(), half)],
        )]);
        let out = frame_at(&mut compositor, &mut cache, &display, Some(&capture), size);
        assert_eq!(
            out.pixel(4, 9),
            Some([0, 0, 255, 255]),
            "the capture on the left"
        );
        assert_eq!(out.pixel(28, 9), Some(RED), "red on the right");
    }

    /// A live source's GPU frame (a browser page) is sampled as premultiplied sRGB by a
    /// compositor on its context, and draws nothing in one with a device of its own, such
    /// as `video.snapshot`'s, even where wgpu's device IDs coincide.
    #[test]
    fn a_gpu_source_frame_draws_only_on_its_own_context() {
        use jstream_media::{ColorInfo, MediaTime, PixelFormat, Timebase, VideoFrameInfo};
        use jstream_platform::{AdapterId, FrameSync, GpuContext, GpuFrame, PlatformError, wgpu};
        use std::sync::Arc;

        #[derive(Debug)]
        struct Ready;
        impl FrameSync for Ready {
            fn stage(&self, _queue: &wgpu::Queue) -> Result<(), PlatformError> {
                Ok(())
            }
        }

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(device) = gpu() else { return };
        let Some(mut other) = gpu() else { return };
        let context = GpuContext::new(
            device.adapter().clone(),
            device.device().clone(),
            device.queue().clone(),
            AdapterId {
                luid: None,
                vendor: 0,
            },
            None,
        );
        let mut own = Compositor::on_context(&context);
        let extent = wgpu::Extent3d {
            width: 16,
            height: 9,
            depth_or_array_layers: 1,
        };
        let texture = own.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("jstream-core-test-page"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Red at alpha 128, premultiplied in code values, in BGRA order.
        let page: Vec<u8> = std::iter::repeat_n([0, 0, 128, 128], 16 * 9)
            .flatten()
            .collect();
        own.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &page,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(16 * 4),
                rows_per_image: Some(9),
            },
            extent,
        );
        let frame = GpuFrame::new(
            texture,
            VideoFrameInfo {
                size: FrameSize::new(16, 9).expect("invariant: non-zero"),
                format: PixelFormat::Bgra8,
                color: ColorInfo::default(),
                pts: MediaTime::zero(Timebase::NANOS),
            },
            Arc::new(Ready),
        );
        let mut page_shot = shot(vec![("program", vec![fill("green"), fill("page")])]);
        let page_id = SourceId::new("page");
        page_shot
            .frames
            .insert(page_id.clone(), Frame::from_gpu(frame, context.key(), 7));
        page_shot
            .uploads
            .push((GpuKey::Frame(page_id.clone()), page_id));
        let canvas = page_shot.canvas().expect("canvas");

        // Half the page's red over half the green, in linear light.
        let mut cache = ResourceCache::new();
        let out = frame_at(&mut own, &mut cache, &page_shot, None, canvas);
        let half = (linear_to_srgb(128.0 / 255.0) * 255.0).round() as u8;
        let rest = (linear_to_srgb(1.0 - 128.0 / 255.0) * 255.0).round() as u8;
        let [r, g, b, a] = out.pixel(960, 540).expect("inside");
        assert!(
            r.abs_diff(half) <= 2 && g.abs_diff(rest) <= 2 && b == 0 && a == 255,
            "{:?}, expected about {:?}",
            [r, g, b, a],
            [half, rest, 0, 255]
        );

        let mut cache = ResourceCache::new();
        let out = frame_at(&mut other, &mut cache, &page_shot, None, canvas);
        assert_eq!(out.pixel(960, 540), Some(GREEN), "the page draws nothing");
    }

    /// The privacy screen blurs the whole picture: the letterbox's sharp edge becomes a
    /// ramp, while flat areas keep their colour. Off, it draws exactly what no screen
    /// does (ADR-0027 §4).
    #[test]
    fn the_privacy_screen_blurs_the_picture() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let square = FrameSize::new(256, 256).expect("invariant: non-zero");
        let shot = shot(vec![("program", vec![fill("green")])]);
        let mut draw = |privacy| {
            cache.begin_frame();
            let output = compose(
                &mut compositor,
                (&mut cache, &mut chains),
                (&shot, zero()),
                View { mix: None, privacy },
                &LiveParams::default(),
                &super::NoCaptures,
                super::DrawAt::read(square),
            )
            .expect("compose");
            drop(cache.end_frame(&compositor));
            output.pixels().expect("invariant: read back").clone()
        };
        let plain = draw(None);
        assert_eq!(draw(Some(false)), plain);
        // 1920x1080 fitted into 256x256 is 256x144, centred: rows 56 to 199.
        assert_eq!(plain.pixel(128, 50), Some([0, 0, 0, 255]));
        assert_eq!(plain.pixel(128, 60), Some(GREEN));

        let covered = draw(Some(true));
        assert_ne!(covered, plain);
        assert_eq!(
            covered.pixel(128, 128),
            Some(GREEN),
            "flat areas keep their colour"
        );
        assert_eq!(covered.pixel(128, 0), Some([0, 0, 0, 255]));
        let ramp: Vec<u8> = (48..64)
            .filter_map(|y| covered.pixel(128, y))
            .map(|[_, g, _, _]| g)
            .collect();
        assert!(
            ramp.windows(2).all(|w| w[0] <= w[1]),
            "the edge rises steadily: {ramp:?}"
        );
        assert!(
            ramp.iter().filter(|g| **g > 0 && **g < 255).count() >= 4,
            "the edge is soft: {ramp:?}"
        );
    }

    /// Frame after frame of the same shot reuses every texture: the nested-scene layers
    /// and the uploads stay in the cache, and nothing is retired. A shot without nesting
    /// retires the layers, which come back once the GPU has finished with them.
    #[test]
    fn nested_scene_layers_are_reused_across_frames() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let nested = chain(2);
        let canvas = nested.canvas().expect("canvas");
        let _ = frame_at(&mut compositor, &mut cache, &nested, None, canvas);
        let layers = [layer("s1", 1), layer("s2", 2)];
        let textures: Vec<_> = layers
            .iter()
            .map(|key| cache.get(key).expect("a layer").texture().clone())
            .collect();
        let live = cache.live();
        for _ in 0..3 {
            let out = frame_at(&mut compositor, &mut cache, &nested, None, canvas);
            assert_eq!(out.pixel(960, 540), Some(GREEN));
        }
        for (key, texture) in layers.iter().zip(&textures) {
            assert!(
                cache.get(key).expect("still cached").texture() == texture,
                "{key:?} kept its texture"
            );
        }
        assert_eq!((cache.live(), cache.pending()), (live, 0));

        let flat = shot(vec![("program", vec![fill("red")])]);
        let out = frame_at(&mut compositor, &mut cache, &flat, None, canvas);
        assert_eq!(out.pixel(960, 540), Some(RED));
        assert!(cache.get(&layers[0]).is_none(), "no longer referenced");
        // The next frame's readback waits for the GPU, which passes the fence of the
        // frame that last used the layers.
        let _ = frame_at(&mut compositor, &mut cache, &flat, None, canvas);
        assert_eq!(cache.pending(), 0, "freed after the frame fence");
    }

    /// Mixes drawn on different ticks keep their layers: on a tick where only the
    /// program (drawn straight at the canvas size) is due, the half-rate mix claims the
    /// program layer it fits from, so nothing is retired or allocated tick after tick.
    #[test]
    fn a_mix_keeps_its_layers_between_the_ticks_it_draws_on() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let nested = chain(2);
        let canvas = nested.canvas().expect("canvas");
        let small = FrameSize::new(32, 18).expect("invariant: non-zero");
        use jstream_protocol::VideoMixId;
        let half = VideoMixId::new("half");
        let mut chains = Chains::default();
        let mut tick = |cache: &mut ResourceCache<GpuKey>, index: u32| {
            cache.begin_frame();
            compose(
                &mut compositor,
                (&mut *cache, &mut chains),
                (&nested, zero()),
                View::default(),
                &LiveParams::default(),
                &super::NoCaptures,
                super::DrawAt::read(canvas),
            )
            .expect("program");
            if index.is_multiple_of(2) {
                compose(
                    &mut compositor,
                    (&mut *cache, &mut chains),
                    (&nested, zero()),
                    View {
                        mix: Some(&half),
                        privacy: None,
                    },
                    &LiveParams::default(),
                    &super::NoCaptures,
                    super::DrawAt::read(small),
                )
                .expect("half");
            } else {
                claim_layers(
                    &mut compositor,
                    (&mut *cache, &mut chains),
                    &nested,
                    Some(&half),
                    small,
                )
                .expect("claim");
            }
            drop(cache.end_frame(&compositor));
        };
        tick(&mut cache, 0);
        let program = cache
            .get(&GpuKey::Program)
            .expect("the mix's layer")
            .texture()
            .clone();
        let live = cache.live();
        for index in 1..6 {
            tick(&mut cache, index);
            assert_eq!((cache.live(), cache.pending()), (live, 0), "tick {index}");
            assert!(
                cache.get(&GpuKey::Program).expect("kept").texture() == &program,
                "tick {index} kept the program layer"
            );
        }
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
        assert_eq!(
            Frame::new(0, 1, &[], 1).unwrap_err(),
            SourceFrameError::Empty
        );
        assert_eq!(
            Frame::new(2, 2, &[0, 0, 0, 0], 1).unwrap_err(),
            SourceFrameError::Length {
                width: 2,
                height: 2,
                expected: 16,
                len: 4,
            }
        );
        let frame = Frame::new(1, 1, &[9, 8, 7, 6], 3).expect("one pixel");
        assert_eq!(frame.rgba(), &[9, 8, 7, 6]);
    }
}
