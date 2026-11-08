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
    Filter, Graph, Item as GraphItem, Output as GraphOutput, Placement as GraphPlacement,
    Scene as GraphScene, Source, State,
};
use jstream_media::model::Lighting;
use jstream_media::{FrameSize, MediaTime, PixelFormat, Rounding, VideoFrame};
use jstream_platform::{ContextKey, GpuFrame};
use jstream_protocol::{
    BlendMode, Blending, CallResult, Canvas, ChainCost, Crop as ProtocolCrop, Edit, FilterChain,
    Glide, ItemGeometry, ItemId, ItemTransition, OutputId, OutputKind, ProtocolError, QueryResult,
    Rev, SceneCamera, SceneId, SceneRole, Settings, SourceId, SourceKind, VideoMixId,
};
use jstream_render::transition::{Endpoint, Timing, Track, lerp_camera};
use jstream_render::{
    Color, Compositor, Crop, Item, LegacyTransform, OpenOptions, Output, Placement, RenderError,
    ResourceCache, Scene,
};
use jstream_sources::{CaptureSpec, ShownModel};

use crate::chain::{Chain, ChainOwner, Chains};
use crate::filter_assets::FilterAssets;
use crate::follow::{CameraMotion, CameraMoves, FollowPlan};
use crate::frames::{self, Sheet};
use crate::lock;
use crate::motion::{self, TrackMotion};
use crate::profile::{DrawMeter, Phase};
use crate::runtime::RenderSnapshot;
use crate::transition::{self, From, Planned, Running, Seen, Visual};

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
    /// The 3D model its source shows (ADR-0107), whose layer an item draws in place of
    /// this frame, the model's transparent stand-in.
    model: Option<ShownModel>,
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
    /// A live source's frame slot (ADR-0117 §3): each draw uploads what it holds then,
    /// by that frame's own version, so a new frame of the same size needs no plan.
    Slot(Arc<crate::slot::FrameSlot>),
}

impl PartialEq for Frame {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.width == other.width
            && self.height == other.height
            && self.extent == other.extent
            && model_key(self.model.as_ref()) == model_key(other.model.as_ref())
    }
}

/// What tells two shown models apart: a new version is a new model to upload, and the
/// lighting is drawn with it. Cheaper than comparing the models.
fn model_key(model: Option<&ShownModel>) -> Option<(u64, Lighting)> {
    model.map(|shown| (shown.model.version(), shown.lighting))
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
            model: None,
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
            model: None,
        })
    }

    /// A live source's frame slot, at the size of the frame it holds now: each draw
    /// shows the slot's latest frame ([`crate::slot`]).
    pub(crate) fn from_slot(slot: Arc<crate::slot::FrameSlot>, version: u64) -> Self {
        let size = slot.size();
        Self {
            width: size.width(),
            height: size.height(),
            extent: [size.width(), size.height()],
            pixels: Pixels::Slot(slot),
            version,
            model: None,
        }
    }

    /// The size it is placed at, which `source.sizes` reports (ADR-0073).
    pub(crate) fn extent(&self) -> Option<FrameSize> {
        FrameSize::new(self.extent[0], self.extent[1]).ok()
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
            model: None,
        }
    }

    /// The same frame as the stand-in of `model`, the 3D model its source shows
    /// (ADR-0107).
    pub(crate) fn with_model(self, model: Option<ShownModel>) -> Self {
        Self { model, ..self }
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
            // Read through the slot at a draw ([`compose`]); none to borrow here.
            Pixels::Gpu { .. } | Pixels::Slot(_) => &[],
        }
    }

    /// Its size in pixels and the size it is placed at, for tests.
    #[cfg(test)]
    pub(crate) fn sizes(&self) -> ([u32; 2], [u32; 2]) {
        ([self.width, self.height], self.extent)
    }

    /// Every pixel, tightly packed, for tests that compare what a source shows.
    #[cfg(test)]
    pub(crate) fn pixels(&self) -> Vec<u8> {
        match &self.pixels {
            // What a draw would upload now.
            Pixels::Slot(slot) => slot.latest().rgba().to_vec(),
            _ => self.rgba().to_vec(),
        }
    }

    /// The first pixel, for tests that check what a source shows.
    #[cfg(test)]
    pub(crate) fn first_pixel(&self) -> Option<[u8; 4]> {
        self.pixels().get(..4)?.try_into().ok()
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
    /// A `scene` source's video chain over the scene it places, planned under that ID
    /// (ADR-0065 §4): [`ChainOwner::Placed`]'s output, kept in [`Shot::chains`].
    Placed(SourceId, SceneId),
    /// An item's box after its effects (ADR-0065), as that scene draws the item: what a
    /// transition's track shows for an item with effects.
    Boxed(SceneId, ItemId),
}

#[derive(Debug, PartialEq)]
struct Placed {
    /// The item, whose live values (ADR-0032) override `placement` and `crop`.
    item: ItemId,
    /// The keyframe scene that draws it, if the scene is one: which live values apply.
    through: Option<Through>,
    source: SourceRef,
    placement: Placement,
    crop: Crop,
    /// The item's straight alpha multiplier (ADR-0023), in [0, 1].
    opacity: f32,
    /// How it composites onto what is below it (ADR-0082).
    blend: BlendMode,
    /// Its effects' box, when an effect of it runs: the layer then shows the effects'
    /// output in place of `source`.
    effects: Option<Boxed>,
    /// A group's child that a Move within a family moves inside the group's layer
    /// (ADR-0049 §3): its track and the transition's start, which replace the fields
    /// above (`placement` holds its end) and its live values.
    moving: Option<Moving>,
    /// Its motion modifiers (ADR-0099), evaluated as each frame is drawn: both sides'
    /// for a moving child, its own on both sides otherwise.
    motion: TrackMotion,
}

/// A layer's track inside a group a transition moves, and when the transition started.
type Moving = (MediaTime, Track);

/// The longest side of an item's effects layer, in pixels: a larger box plus outset is
/// drawn into it scaled down uniformly (ADR-0065 §5).
const MAX_LAYER_SIDE: u32 = 8192;

/// The layer an item with effects is drawn into (ADR-0065 §5): its box as planned, in
/// canvas pixels, scaled down to fit [`MAX_LAYER_SIDE`] with the effects' outset on
/// every side when larger. The effects grow it by the outset where the first of them
/// that draws past its input runs (`Chain::pad_at`), and the result is placed over the
/// box grown by the outset. A live resize draws into the same layer, scaled, until
/// the plan is made again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Boxed {
    /// The box as planned, in canvas pixels.
    size: [f32; 2],
    /// The room the effects need beyond each edge, in canvas pixels.
    outset: f32,
    /// The size in pixels of the layer the item is drawn into: the box's.
    layer: [u32; 2],
    /// Layer pixels per canvas pixel: 1, or less for a layer scaled down to fit.
    scale: f32,
    /// The size of the image the item shows, when known: what turns a legacy transform
    /// into its box.
    input: Option<[u32; 2]>,
    /// Whether the effects run over the canvas after the item is placed (ADR-0106): the
    /// layer is then the canvas, `size` is the canvas's and `outset` is 0.
    canvas: bool,
    /// Whether the item shows a 3D model (ADR-0107 decision 6): its layer is the
    /// canvas, which the model is drawn into through the scene's camera, and it is
    /// placed flat as [`Placement::Layer`], sorting by the model's pose.
    model: bool,
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
            // The box alone: the effects grow it by the outset where the first of them
            // that draws past its input runs (`Chain::pad_at`).
            layer: [side(size[0]), side(size[1])],
            scale,
            input,
            canvas: false,
            model: false,
        })
    }

    /// This box with effects that need `outset` pixels instead.
    fn with_outset(self, outset: u32) -> Option<Self> {
        let outset = outset as f32;
        let padded = [self.size[0] + 2.0 * outset, self.size[1] + 2.0 * outset];
        if !padded.iter().all(|side| side.is_finite() && *side > 0.0) {
            return None;
        }
        let scale = (MAX_LAYER_SIDE as f32 / padded[0].max(padded[1])).min(1.0);
        let side = |px: f32| ((px * scale).ceil() as u32).clamp(1, MAX_LAYER_SIDE);
        Some(Self {
            outset,
            layer: [side(self.size[0]), side(self.size[1])],
            scale,
            ..self
        })
    }

    /// The layer for an item whose effects run in canvas space (ADR-0106): the canvas,
    /// scaled down to fit [`MAX_LAYER_SIDE`] when larger, with no outset, since what the
    /// effects draw outside the canvas is never seen.
    fn over_canvas(canvas: Canvas, input: Option<[u32; 2]>) -> Option<Self> {
        let size = [canvas.width as f32, canvas.height as f32];
        if !size.iter().all(|side| side.is_finite() && *side > 0.0) {
            return None;
        }
        let scale = (MAX_LAYER_SIDE as f32 / size[0].max(size[1])).min(1.0);
        let side = |px: f32| ((px * scale).ceil() as u32).clamp(1, MAX_LAYER_SIDE);
        Some(Self {
            size,
            outset: 0.0,
            layer: [side(size[0]), side(size[1])],
            scale,
            input,
            canvas: true,
            model: false,
        })
    }

    /// The box `placement` puts an item cropped by `crop` in: its geometry, or a legacy
    /// transform's equivalent over the image's size. `None` when that isn't known or the
    /// placement draws nothing.
    fn geometry_of(&self, placement: Placement, crop: Crop) -> Option<ItemGeometry> {
        match placement {
            Placement::Geometry(geometry) => Some(geometry),
            // A model's layer covers the canvas; its box is the model's pose, not where
            // the layer's pixels are, so it has none to pair by.
            Placement::Layer(_) => None,
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

    /// What `by`, an item's motion, moves inside its layer: its crop, cut there, and over
    /// the canvas (ADR-0106) its placement too, since that layer stays where it is and
    /// [`Self::around`] drops the box. Else the rest moves the box where it is placed.
    fn moved_inside(
        &self,
        by: &jstream_anim::Offsets,
        placement: Placement,
        crop: Crop,
    ) -> (Placement, Crop) {
        let (moved, crop, _) = motion::apply(by, placement, crop, OPAQUE);
        (if self.canvas { moved } else { placement }, crop)
    }

    /// Where the item is drawn inside its layer: centred at the box's planned size,
    /// flat and unrotated, with `geometry`'s fit and flips, so the effects see the
    /// cropped, fitted box in its own frame (ADR-0050 §7).
    fn inside(&self, geometry: &ItemGeometry) -> Option<ItemGeometry> {
        if self.canvas {
            // Placed where it is on the canvas, turned, tilted and flipped as it is, in
            // layer pixels.
            let px = |value: f32| jstream_protocol::Finite32::new(value * self.scale);
            return Some(ItemGeometry {
                center: jstream_protocol::Vec2::new(
                    geometry.center.x.get() * self.scale,
                    geometry.center.y.get() * self.scale,
                )?,
                size: jstream_protocol::Size::try_new(
                    geometry.size.width.get() * self.scale,
                    geometry.size.height.get() * self.scale,
                )
                .ok()?,
                depth_px: px(geometry.depth_px.get())?,
                perspective_px: match geometry.perspective_px {
                    Some(perspective) => Some(px(perspective.get())?),
                    None => None,
                },
                ..*geometry
            });
        }
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
        if self.canvas {
            // The whole canvas, flat: the item's own transform is inside the layer.
            return Some(ItemGeometry {
                fit_mode: jstream_protocol::FitMode::Stretch,
                ..ItemGeometry::covering_canvas(self.size[0] as u32, self.size[1] as u32).ok()?
            });
        }
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
    /// The keyframe scene that draws it, if the scene is one: which live values apply.
    through: Option<Through>,
    source: SourceRef,
    placement: Placement,
    crop: Crop,
    boxed: Boxed,
    /// Its motion modifiers (ADR-0099): their crop is cut inside the box, and the rest
    /// moves the box where it is placed, except over the canvas (ADR-0106), where they
    /// move the item inside its layer ([`Boxed::moved_inside`]).
    motion: motion::Motion,
    /// For a model's layer (ADR-0107), the camera of the scene it is in, which the
    /// model is drawn through, as a pass resolves its own; `None` in a 2D scene.
    camera: Option<PassCamera>,
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
    /// The space every scene of the collection mixes in (ADR-0126).
    blending: Blending,
    /// The scene on program (ADR-0030). `None` when the collection has no scene.
    program: Option<SceneId>,
    scenes: BTreeMap<SceneId, Vec<Placed>>,
    /// The background each copied scene has, by scene, straight sRGB (ADR-0089 §4): a
    /// keyframe scene's is its parent's, and a group, or a group's view, has none. A
    /// scene without one isn't here, and is transparent.
    backgrounds: BTreeMap<SceneId, Color>,
    /// The camera each copied 3D scene draws through (ADR-0094), by scene. A scene
    /// without one isn't here, and is 2D.
    cameras: BTreeMap<SceneId, PassCamera>,
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
    /// The output stage's video chains (ADR-0080): the master stage's over each mix an
    /// output may draw, and each output's own, with the image each reads. They run as
    /// an output's frame is drawn ([`compose`] with [`View::stage`]), not in the chain
    /// phase, and never on the editing canvas or a preview.
    stages: BTreeMap<ChainOwner, Chain>,
    /// Whether an item it draws has a motion modifier (ADR-0099), so what it draws
    /// changes with time.
    moves: bool,
    /// When each item it draws was first drawn, the origin of its `shown` clock
    /// (ADR-0099), as the engine set it ([`Shot::set_shown`]). An item missing here is
    /// at its start.
    shown: BTreeMap<ItemId, MediaTime>,
    /// Its items' audio modifiers (ADR-0100), with the level the render thread's
    /// follower stores for each tick ([`crate::reactive`]).
    audio: crate::reactive::Bindings,
    /// When each item's `triggered` modifiers were last fired (#1107), as the engine set
    /// it ([`Shot::set_fired`]). A modifier never fired here is still.
    fired: motion::Fired,
}

/// A transition as a shot draws it (ADR-0029): each item's track in drawing order and
/// the image it shows, sampled at the frame's time since `start`.
#[derive(Debug, PartialEq)]
struct Transitioning {
    start: MediaTime,
    /// Each track with the camera its visual is seen through, and its motion modifiers
    /// (ADR-0099).
    tracks: Vec<(GpuKey, Track, Seen, TrackMotion)>,
    /// The backgrounds it fades between, from the scene it leaves to the one it shows,
    /// over `end_ms`.
    backgrounds: (Color, Color),
    /// The cameras its tracks are seen through.
    cameras: TrackCameras,
    /// What moves the cameras of the scene it leaves and the one it shows (ADR-0113 §3):
    /// a Move into or out of a follow camera moves with it.
    moves: (Option<Arc<CameraMoves>>, Option<Arc<CameraMoves>>),
    /// The item chains whose settings a Move blends between two looks (ADR-0127).
    blends: Vec<EffectBlend>,
    end_ms: f32,
}

/// One item's effects as a Move blends their settings (ADR-0127): from the leaving
/// side's to the arriving chain's, with the item's own progress.
#[derive(Debug, PartialEq)]
struct EffectBlend {
    /// The arriving item's chain, which draws the pair.
    owner: ChainOwner,
    /// The settings it blends from, one per filter of the chain.
    from: Arc<[Settings]>,
    /// The settings it blends to: the chain's own, one per filter.
    to: Vec<Settings>,
    /// Each filter's `int` parameters, which stay whole.
    whole: Vec<Vec<String>>,
    /// The item's timing, whose eased progress the blend follows.
    timing: Timing,
}

impl Transitioning {
    /// Each blended chain's settings `at`, one per filter (ADR-0127).
    fn blended(&self, at: MediaTime) -> Vec<(&ChainOwner, Vec<Settings>)> {
        self.blended_ms(transition::elapsed_ms(self.start, at))
    }

    /// [`Self::blended`], `elapsed_ms` after its start.
    fn blended_ms(&self, elapsed_ms: f32) -> Vec<(&ChainOwner, Vec<Settings>)> {
        self.blends
            .iter()
            .map(|blend| {
                let k = blend.timing.progress(elapsed_ms);
                let settings = blend
                    .from
                    .iter()
                    .zip(&blend.to)
                    .zip(&blend.whole)
                    .map(|((from, to), whole)| {
                        jstream_render::transition::blend_settings(from, to, k, whole)
                    })
                    .collect();
                (&blend.owner, settings)
            })
            .collect()
    }
}

/// The blends of `tracks` and of the children moving inside each group among them
/// (ADR-0127), for the chains `chains` holds.
fn effect_blends(
    tracks: &[(Visual, Track)],
    chains: &BTreeMap<ChainOwner, Chain>,
    blends: &mut Vec<EffectBlend>,
) {
    for (visual, track) in tracks {
        if let Some(inside) = &visual.inside {
            effect_blends(&inside.planned.tracks, chains, blends);
        }
        let (Some(from), SourceRef::Boxed(scene, item), Track::Pair { timing, .. }) =
            (&visual.blend_from, &visual.image, track)
        else {
            continue;
        };
        let owner = ChainOwner::Item(scene.clone(), item.clone());
        let Some(chain) = chains.get(&owner) else {
            continue;
        };
        if chain.filters.len() != from.len() {
            continue;
        }
        blends.push(EffectBlend {
            to: chain.filters.iter().map(|f| f.settings.clone()).collect(),
            whole: chain.whole_params(),
            owner,
            from: Arc::clone(from),
            timing: *timing,
        });
    }
}

/// Grows the layer of `owner`'s item, and its chain's pad, to hold what its effects
/// draw at the settings `from` a Move blends from as well as at their own: an effect's
/// outset follows its settings (ADR-0136), and the blend passes through
/// both looks.
fn widen(shot: &mut Shot, owner: &ChainOwner, from: &[Settings]) {
    let (Some(chain), Some(planned)) = (shot.chains.get_mut(owner), shot.boxes.get_mut(owner))
    else {
        return;
    };
    let boxed = planned.boxed;
    let outset = chain.outset_also(Some(from));
    if boxed.canvas || boxed.model || outset as f32 <= boxed.outset {
        return;
    }
    let Some(boxed) = boxed.with_outset(outset) else {
        return;
    };
    planned.boxed = boxed;
    chain.pad = chain.pad_at_also(boxed.scale, Some(from));
}

/// The cameras a transition's tracks are seen through (ADR-0094 decision 7).
#[derive(Clone, Copy, Debug, PartialEq)]
struct TrackCameras {
    /// The cameras it moves between, from the scene it leaves (or the one camera of what
    /// was on screen) to the one it shows, `None` for a 2D side.
    ends: (Option<SceneCamera>, Option<SceneCamera>),
    /// The camera a stinger's scene is seen through, over both sides: its own, `None`
    /// for a 2D stinger scene or another kind.
    overlay: Option<SceneCamera>,
    /// The preset's own timing, which a moving camera follows.
    timing: Timing,
    /// Whether it is a Move, whose tracks share one moving camera; a Fade's leaving
    /// tracks keep the camera they leave from.
    pairs: bool,
    /// The depth run a stinger's items sort in: one past every run held from the
    /// screen, so a stinger's never sort with those of one it cut short.
    overlay_run: u32,
}

impl TrackCameras {
    /// The camera `track` is seen through `elapsed_ms` into the transition: a Move's
    /// tracks share the camera moving between the two ends; a Fade's leaving tracks keep
    /// the one they leave from, and the rest take the one it shows. A stinger's items
    /// take their own scene's. Outside a Move, what was taken from the screen keeps the
    /// camera it was seen through then (`seen`); a Move only starts from a screen seen
    /// through one camera, its first end, so all its tracks share the moving one.
    fn camera(&self, track: &Track, seen: Seen, elapsed_ms: f32) -> Option<SceneCamera> {
        if matches!(track, Track::Through { .. }) {
            return self.overlay;
        }
        if let Seen::Held { camera, .. } = seen
            && !self.pairs
        {
            return camera;
        }
        let (from, to) = self.ends;
        if !self.pairs {
            return match track {
                Track::Disappear { .. } => from,
                _ => to,
            };
        }
        match (from, to) {
            (Some(from), Some(to)) if from != to => {
                let k = self.timing.progress(elapsed_ms);
                Some(lerp_camera(&from, &to, k))
            }
            _ => to,
        }
    }
}

impl TrackCameras {
    /// The depth run `track` sorts in (ADR-0094 decision 4): a stinger's items one of
    /// their own (`overlay_run`), kept when a switch takes them from the screen, so they
    /// stay over the scenes under them through an equal camera; everything else `0`.
    fn depth_run(&self, track: &Track, seen: Seen) -> u32 {
        match seen {
            Seen::Held { depth_run, .. } => depth_run,
            Seen::AsPlanned if matches!(track, Track::Through { .. }) => self.overlay_run,
            Seen::AsPlanned => 0,
        }
    }
}

impl Transitioning {
    /// The background shown under its tracks at `at`: the scene it leaves's, mixed
    /// toward the shown scene's as the transition runs.
    fn background(&self, at: MediaTime) -> Color {
        let (from, to) = self.backgrounds;
        if from == to || self.end_ms <= 0.0 {
            return to;
        }
        match transition::elapsed_ms(self.start, at) / self.end_ms {
            t if t >= 1.0 => to,
            t if t <= 0.0 => from,
            t => from.mix(to, t),
        }
    }
}

impl Default for Shot {
    /// No scene: the primary display's capture fills the output.
    fn default() -> Self {
        Self {
            canvas: Canvas::default(),
            blending: Blending::default(),
            program: None,
            scenes: BTreeMap::new(),
            backgrounds: BTreeMap::new(),
            cameras: BTreeMap::new(),
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
            stages: BTreeMap::new(),
            moves: false,
            shown: BTreeMap::new(),
            audio: crate::reactive::Bindings::default(),
            fired: motion::Fired::default(),
        }
    }
}

impl Shot {
    /// What the copied scene `id` clears to under its items: its background, or
    /// transparent.
    fn background(&self, id: &SceneId) -> Color {
        self.backgrounds
            .get(id)
            .copied()
            .unwrap_or(Color::TRANSPARENT)
    }

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

    /// What the frame slots this shot draws hold now ([`crate::slot`]): it changes when
    /// any of them takes a new frame, so a stream that skips an unchanged picture draws
    /// one that changed only there. Zero with no slot.
    pub(crate) fn slot_stamp(&self) -> u64 {
        self.frames
            .values()
            .filter_map(|frame| match &frame.pixels {
                Pixels::Slot(slot) => Some(slot.latest().version()),
                _ => None,
            })
            .fold(0, u64::wrapping_add)
    }

    /// What each of its frame slots holds now (ADR-0117 §3): a frame sheet draws its
    /// pictures later, and each must show the frames it was taken with.
    pub(crate) fn pin(&self) -> Pinned {
        Pinned(
            self.frames
                .iter()
                .filter_map(|(source, frame)| match &frame.pixels {
                    Pixels::Slot(slot) => Some((source.clone(), slot.latest())),
                    _ => None,
                })
                .collect(),
        )
    }

    /// Whether what this shot draws may change with time alone: a transition runs, a
    /// source has a video chain, whose filters may move or hold frames (scroll, video
    /// delay), or an item it draws has motion (ADR-0099). Every chain counts, which
    /// redraws a still filtered picture too.
    pub(crate) fn changes_with_time(&self) -> bool {
        self.transitioning() || !self.chains.is_empty() || !self.stages.is_empty() || self.moves
    }

    /// Whether `image` is the layer of an item that shows a 3D model (ADR-0107): drawn
    /// in the canvas's pixels, so placed flat as [`Placement::Layer`].
    fn is_model_box(&self, image: &GpuKey) -> bool {
        let owner = match image {
            GpuKey::Filtered(owner @ ChainOwner::Item(..)) => owner.clone(),
            GpuKey::ItemBox(scene, item) => ChainOwner::Item(scene.clone(), item.clone()),
            _ => return false,
        };
        self.boxes
            .get(&owner)
            .is_some_and(|boxed| boxed.boxed.model)
    }

    /// Whether `image` is the layer of an item whose effects run over the canvas
    /// (ADR-0106).
    fn is_canvas_box(&self, image: &GpuKey) -> bool {
        // A track or layer draws a box through its chain's output, `Filtered`; its
        // `ItemBox` is the layer the item is drawn into first (#1125 review N16).
        let owner = match image {
            GpuKey::Filtered(owner @ ChainOwner::Item(..)) => owner.clone(),
            GpuKey::ItemBox(scene, item) => ChainOwner::Item(scene.clone(), item.clone()),
            _ => return false,
        };
        self.boxes
            .get(&owner)
            .is_some_and(|boxed| boxed.boxed.canvas)
    }

    /// Whether an item this shot draws has a motion modifier: in a pass, a chain
    /// phase's pass, a box or a transition track.
    fn has_motion(&self) -> bool {
        let chain_passes = self.chain_steps.iter().filter_map(|step| match step {
            ChainStep::Pass(pass) => Some(&**pass),
            ChainStep::Run(_) | ChainStep::Box(_) => None,
        });
        self.passes
            .iter()
            .chain(self.mix_passes.values().flatten())
            .chain(chain_passes)
            .flat_map(|pass| &pass.layers)
            .any(|layer| !layer.motion.is_still())
            || self.boxes.values().any(|boxed| !boxed.motion.is_empty())
            || self
                .transition
                .iter()
                .flat_map(|t| &t.tracks)
                .any(|(_, _, _, motion)| !motion.is_still())
            || self.cameras_move()
    }

    /// Whether a camera it draws through moves on its own (ADR-0113): a follow camera,
    /// or camera motion.
    fn cameras_move(&self) -> bool {
        let moves = |moves: &Option<Arc<CameraMoves>>| moves.as_ref().is_some_and(|m| m.moves());
        self.cameras.values().any(|camera| moves(&camera.moves))
            || self
                .transition
                .as_ref()
                .is_some_and(|t| moves(&t.moves.0) || moves(&t.moves.1))
    }

    /// When the items of `pass` were first drawn: the earliest, the origin of its
    /// camera's `shown` clock (ADR-0113 §1).
    fn pass_shown(&self, pass: &Pass) -> Option<MediaTime> {
        pass.layers
            .iter()
            .filter_map(|layer| self.shown_at(&layer.item))
            .min_by(|a, b| a.as_secs_f64().total_cmp(&b.as_secs_f64()))
    }

    /// Every item this shot draws, in any pass or transition track: whose `shown`
    /// clock runs (ADR-0099).
    pub(crate) fn drawn_items(&self) -> BTreeSet<ItemId> {
        let layers = self
            .passes
            .iter()
            .chain(self.mix_passes.values().flatten())
            .flat_map(|pass| &pass.layers)
            .map(|layer| &layer.item);
        let tracks = self
            .transition
            .iter()
            .flat_map(|t| &t.tracks)
            .flat_map(|(_, _, _, motion)| [&motion.from.0, &motion.to.0]);
        layers
            .chain(tracks)
            .filter(|item| !item.as_str().is_empty())
            .cloned()
            .collect()
    }

    /// Sets when each item it draws was first drawn ([`motion::ShownSince`]).
    pub(crate) fn set_shown(&mut self, shown: BTreeMap<ItemId, MediaTime>) {
        self.shown = shown;
    }

    /// When `item` was first drawn, if this shot knows.
    fn shown_at(&self, item: &ItemId) -> Option<MediaTime> {
        self.shown.get(item).copied()
    }

    /// Its items' audio modifiers, whose levels the render thread's follower sets once a
    /// tick (ADR-0100).
    pub(crate) fn audio(&self) -> &crate::reactive::Bindings {
        &self.audio
    }

    /// The level of `item`'s audio modifier `modifier` this tick.
    fn heard(&self, item: &ItemId, modifier: &str) -> Option<f32> {
        self.audio.level(item, modifier)
    }

    /// Sets when each item's `triggered` modifiers were last fired ([`motion::Fired`]).
    pub(crate) fn set_fired(&mut self, fired: motion::Fired) {
        self.fired = fired;
    }

    /// What `item`'s `motion` adds at `at`, on its clocks as this shot knows them.
    fn motion_at(
        &self,
        item: &ItemId,
        motion: &[jstream_anim::Modifier],
        at: MediaTime,
    ) -> jstream_anim::Offsets {
        motion::offsets(motion, at, self.shown_at(item), self.fired.of(item), |id| {
            self.heard(item, id)
        })
    }

    /// The output stage's chains in this shot, for tests.
    #[cfg(test)]
    pub(crate) fn stage_chain(&self, owner: &ChainOwner) -> Option<&Chain> {
        self.stages.get(owner)
    }

    /// The output stage an output's frame of `mix` goes through (ADR-0080): the mix's
    /// layer, the master chain over it and `owner`'s own chain, or `None` when neither
    /// chain runs, so the mix is drawn straight to the output as before.
    fn stage_of(&self, mix: Option<&VideoMixId>, owner: Option<&OutputId>) -> Option<Stage<'_>> {
        let master_owner = ChainOwner::Master(mix.cloned());
        let master = self.stages.get_key_value(&master_owner);
        let output = owner.and_then(|id| {
            self.stages
                .get_key_value(&ChainOwner::Output(id.clone()))
                // Drawn after this mix's master stage only: an output whose mix changed
                // since it started is drawn without it until its next start.
                .filter(|(_, chain)| match (&chain.input, master) {
                    (GpuKey::Filtered(read), Some(_)) => *read == master_owner,
                    (GpuKey::Mixed(read), None) => read.as_ref() == mix,
                    _ => false,
                })
        });
        if master.is_none() && output.is_none() {
            return None;
        }
        Some(Stage {
            mixed: GpuKey::Mixed(mix.cloned()),
            master,
            output,
        })
    }

    /// How many sources have a video chain in this shot, for tests.
    #[cfg(test)]
    pub(crate) fn chain_count(&self) -> usize {
        self.chains.len()
    }

    /// The effect-pack kinds this shot's chains draw, its sources', scenes' and items'
    /// and its master and output stages' (ADR-0080), which a GPU device loss while it is
    /// drawn is blamed on (#349).
    pub(crate) fn pack_kinds(&self) -> std::collections::BTreeSet<String> {
        self.chains
            .values()
            .chain(self.stages.values())
            .flat_map(|chain| chain.pack_kinds())
            .map(str::to_owned)
            .collect()
    }

    /// The video chain of `source_id` in this shot, for tests.
    #[cfg(test)]
    pub(crate) fn chain(&self, source_id: &SourceId) -> Option<&Chain> {
        self.chains.get(&ChainOwner::Source(source_id.clone()))
    }

    /// The video chain `scene` source `source_id` runs over scene `scene_id` where it
    /// places it, for tests.
    #[cfg(test)]
    pub(crate) fn placed_chain(&self, source_id: &SourceId, scene_id: &SceneId) -> Option<&Chain> {
        self.chains
            .get(&ChainOwner::Placed(source_id.clone(), scene_id.clone()))
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

    /// The scenes the chain phase composites, with their depths, in order, for tests.
    #[cfg(test)]
    pub(crate) fn chain_passes(&self) -> Vec<(String, usize)> {
        self.chain_steps
            .iter()
            .filter_map(|step| match step {
                ChainStep::Pass(pass) => Some((pass.scene.to_string(), pass.depth)),
                _ => None,
            })
            .collect()
    }

    /// How many tracks the running transition draws, or `None` without one, for tests.
    #[cfg(test)]
    pub(crate) fn transition_tracks(&self) -> Option<usize> {
        self.transition.as_ref().map(|t| t.tracks.len())
    }

    /// The camera each of the running transition's tracks is seen through
    /// `elapsed_ms` into it, in drawing order, or `None` without one, for tests.
    #[cfg(test)]
    pub(crate) fn track_cameras(&self, elapsed_ms: f32) -> Option<Vec<Option<SceneCamera>>> {
        self.transition.as_ref().map(|t| {
            t.tracks
                .iter()
                .map(|(_, track, seen, _)| t.cameras.camera(track, *seen, elapsed_ms))
                .collect()
        })
    }

    /// The settings each chain a running Move blends draws with `elapsed_ms` after its
    /// start (ADR-0127), or `None` without a transition.
    #[cfg(test)]
    pub(crate) fn blended_effects(
        &self,
        elapsed_ms: f32,
    ) -> Option<Vec<(ChainOwner, Vec<Settings>)>> {
        let transition = self.transition.as_ref()?;
        Some(
            transition
                .blended_ms(elapsed_ms)
                .into_iter()
                .map(|(owner, settings)| (owner.clone(), settings))
                .collect(),
        )
    }

    /// The depth run each of the running transition's tracks sorts in, in drawing order,
    /// or `None` without one, for tests.
    #[cfg(test)]
    pub(crate) fn track_depth_runs(&self) -> Option<Vec<u32>> {
        self.transition.as_ref().map(|t| {
            t.tracks
                .iter()
                .map(|(_, track, seen, _)| t.cameras.depth_run(track, *seen))
                .collect()
        })
    }

    /// The running transition's tracks in drawing order, or `None` without one, for
    /// tests.
    #[cfg(test)]
    pub(crate) fn tracks(&self) -> Option<Vec<Track>> {
        self.transition
            .as_ref()
            .map(|t| t.tracks.iter().map(|(_, track, _, _)| *track).collect())
    }

    /// The layers a transition moves inside a group's layer (ADR-0049 §3), each item
    /// with its track, ordered by the scene that holds them, for tests.
    #[cfg(test)]
    pub(crate) fn moving(&self) -> Vec<(ItemId, Track)> {
        self.scenes
            .values()
            .flatten()
            .filter_map(|p| p.moving.map(|(_, track)| (p.item.clone(), track)))
            .collect()
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

    /// What the program pass of scene `id` draws at `at`, bottom first, with each
    /// item's motion (ADR-0099) and no live values: each item with its placement, crop
    /// and opacity, or `None` when no program pass draws the scene, for tests.
    #[cfg(test)]
    pub(crate) fn moved_at(
        &self,
        id: &SceneId,
        at: MediaTime,
    ) -> Option<Vec<(ItemId, Placement, Crop, f32)>> {
        let pass = self.passes.iter().find(|pass| pass.scene == *id)?;
        Some(
            pass.layers
                .iter()
                .map(|layer| {
                    let (item, modifiers) = &layer.motion.to;
                    let by = self.motion_at(item, modifiers, at);
                    let (placement, crop, opacity) =
                        motion::apply(&by, layer.placement, layer.crop, layer.opacity);
                    (layer.item.clone(), placement, crop, opacity)
                })
                .collect(),
        )
    }

    /// The running transition's tracks sampled `elapsed_ms` into it at `at`, with their
    /// motion (ADR-0099), in drawing order, or `None` without one, for tests.
    #[cfg(test)]
    pub(crate) fn tracks_moved_at(
        &self,
        elapsed_ms: f32,
        at: MediaTime,
    ) -> Option<Vec<Option<jstream_render::transition::Endpoint>>> {
        let canvas = canvas_size(self.canvas);
        self.transition.as_ref().map(|t| {
            t.tracks
                .iter()
                .map(|(key, track, _, moved)| {
                    let by =
                        moved.offsets(track, elapsed_ms, |item, m| self.motion_at(item, m, at));
                    track
                        .sample(elapsed_ms, canvas)
                        .map(|end| moved_track(self.is_canvas_box(key), &by, end))
                })
                .collect()
        })
    }

    /// When each item this shot draws was first drawn, for tests.
    #[cfg(test)]
    pub(crate) fn shown(&self) -> &BTreeMap<ItemId, MediaTime> {
        &self.shown
    }

    /// When `item`'s `triggered` modifier `id` was last fired, for tests.
    #[cfg(test)]
    pub(crate) fn fired_at(&self, item: &ItemId, id: &str) -> Option<MediaTime> {
        self.fired.of(item)?.at(id)
    }

    /// What scene `id` places, bottom first, as it is drawn with the live values in
    /// `live`: each item with its placement and crop, or `None` when the shot doesn't
    /// draw the scene, for tests.
    #[cfg(test)]
    pub(crate) fn drawn(
        &self,
        id: &SceneId,
        live: &LiveParams,
    ) -> Option<Vec<(ItemId, Placement, Crop)>> {
        self.scenes.get(id).map(|placed| {
            placed
                .iter()
                .map(|p| {
                    let (placement, crop) =
                        live.apply_to(&p.item, p.through.as_ref(), p.placement, p.crop);
                    (p.item.clone(), placement, crop)
                })
                .collect()
        })
    }
}

/// Two shots are equal when they draw the same thing: the passes and keys follow from
/// the rest.
impl PartialEq for Shot {
    fn eq(&self, other: &Self) -> bool {
        self.canvas == other.canvas
            && self.blending == other.blending
            && self.program == other.program
            && self.mixes == other.mixes
            && self.scenes == other.scenes
            && self.backgrounds == other.backgrounds
            && self.cameras == other.cameras
            && self.frames == other.frames
            && self.chains == other.chains
            && self.captures == other.captures
            && self.transition == other.transition
            && self.boxes == other.boxes
            && self.stages == other.stages
            && self.moves == other.moves
            && self.shown == other.shown
            && self.audio == other.audio
            && self.fired == other.fired
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
    plan_inner(graph, program, running, look, assets, true)
}

/// Copies the scene `scene_id` and the scenes it nests out of `graph` as the program
/// scene of a shot of its own, with no transition and no video mix, for a preview
/// stream of that scene (ADR-0045). The preview thread captures the displays and
/// windows it shows itself; the program stage never reads this shot.
pub(crate) fn plan_scene(
    graph: &Graph,
    scene_id: &SceneId,
    look: &mut LookUp<'_>,
    assets: &FilterAssets,
) -> Planning {
    plan_inner(graph, Some(scene_id), None, look, assets, false)
}

/// The scene and item a source's preview stream draws it through, in a copy of the
/// collection, each with a number added while the collection holds that ID.
const SOURCE_SCENE: &str = "jstream-source-preview";
const SOURCE_ITEM: &str = "jstream-source-preview-item";

/// The copies of the collection [`plan_source`] plans each previewed source on, one per
/// source, each kept for the revision it was made at. The collection's state changes
/// only through [`Graph::apply`], which moves the revision on, so a re-plan at the same
/// revision (a new frame, an installed asset, a timed wake) reuses the copy instead of
/// copying the whole collection again under the engine's lock. A collection replaced
/// wholesale, not edited, is a new revision line: [`Self::clear`] forgets every copy.
#[derive(Default)]
pub(crate) struct SourceCopies {
    copies: BTreeMap<SourceId, SourceCopy>,
    /// How many copies have been made, for the tests to tell a reuse from a rebuild.
    #[cfg(test)]
    built: usize,
}

/// One source's copy: the revision of the collection it was made from, and the copy
/// with the source's own scene added, or `None` when the copy refused that scene (it
/// is not tried again at that revision).
struct SourceCopy {
    rev: Rev,
    planned: Option<(Graph, SceneId)>,
}

impl SourceCopies {
    /// Lets go of the copies of the sources not in `previewed`.
    pub(crate) fn retain(&mut self, previewed: &BTreeSet<SourceId>) {
        self.copies.retain(|id, _| previewed.contains(id));
    }

    /// Forgets every copy, for a collection replaced rather than edited, whose
    /// revisions may repeat the old one's.
    pub(crate) fn clear(&mut self) {
        self.copies.clear();
    }

    /// The copy of `graph` for `source_id` at `graph`'s revision, made now if there is
    /// none yet.
    ///
    /// Kind changes don't move the revision: an effect pack's poll changes the graph's
    /// kinds (`Graph::with_kinds`, `without_kinds`) with no edit, so a copy can keep the
    /// kinds it was made with. That is safe only because planning reads a pack kind's
    /// effect from `assets.effects()`, never from the graph's kinds. A plan that read the
    /// kinds would need the copy made again when they change.
    fn copy(&mut self, graph: &Graph, source_id: &SourceId) -> Option<(&Graph, &SceneId)> {
        let rev = graph.rev();
        let stale = self
            .copies
            .get(source_id)
            .is_none_or(|copy| copy.rev != rev);
        if stale {
            #[cfg(test)]
            {
                self.built += 1;
            }
            let planned = source_copy(graph, source_id);
            self.copies
                .insert(source_id.clone(), SourceCopy { rev, planned });
        }
        self.copies
            .get(source_id)
            .and_then(|copy| copy.planned.as_ref())
            .map(|(graph, scene_id)| (graph, scene_id))
    }
}

/// Plans the source `source_id` alone, for a preview stream of it (a projector of the
/// source): on a copy of `graph`, one scene of its own whose one item places the source
/// over the whole canvas, as an item filling it would show it, so the source's own
/// filters draw and no item's do. Like [`plan_scene`], nothing here reaches the
/// collection or the program's plan. The copy comes from `copies`, made again only when
/// the collection's revision has moved on. `None` when the copy refuses the scene, which
/// a source the collection holds never makes it do.
pub(crate) fn plan_source(
    copies: &mut SourceCopies,
    graph: &Graph,
    source_id: &SourceId,
    look: &mut LookUp<'_>,
    assets: &FilterAssets,
) -> Option<Planning> {
    let (own, scene_id) = copies.copy(graph, source_id)?;
    Some(plan_inner(own, Some(scene_id), None, look, assets, false))
}

/// A copy of `graph` with one scene added whose one item places `source_id` over the
/// whole canvas, and that scene: the scene and item IDs each get a number added while
/// the collection holds them.
fn source_copy(graph: &Graph, source_id: &SourceId) -> Option<(Graph, SceneId)> {
    let canvas = graph.state().canvas();
    let geometry = ItemGeometry::covering_canvas(canvas.width, canvas.height).ok()?;
    let mut scene_id = SceneId::new(SOURCE_SCENE);
    let mut n = 1;
    while graph.state().scene(&scene_id).is_some() {
        n += 1;
        scene_id = SceneId::new(format!("{SOURCE_SCENE}-{n}"));
    }
    let mut item_id = ItemId::new(SOURCE_ITEM);
    let mut n = 1;
    while graph.state().item(&item_id).is_some() {
        n += 1;
        item_id = ItemId::new(format!("{SOURCE_ITEM}-{n}"));
    }
    let mut own = graph.clone();
    let edits = [
        Edit::SceneCreate {
            scene_id: scene_id.clone(),
            name: "Source preview".into(),
            role: None,
        },
        Edit::ItemAdd {
            scene_id: scene_id.clone(),
            item_id,
            source_id: source_id.clone(),
            transform: None,
            geometry: Some(geometry),
            opacity: None,
            blend: None,
        },
    ];
    own.apply(&edits, None).ok()?;
    Some((own, scene_id))
}

/// [`plan_with`], drawing the scene each `scene` video mix shows when `mixes`.
fn plan_inner(
    graph: &Graph,
    program: Option<&SceneId>,
    running: Option<(&Running, MediaTime)>,
    look: &mut LookUp<'_>,
    assets: &FilterAssets,
    mixes: bool,
) -> Planning {
    let state = graph.state();
    let program = program.cloned();
    let mut shot = Shot {
        canvas: state.canvas(),
        blending: state.blending(),
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
        let (planned, pairs) = planner.transition(running, &mut shot);
        let end = end_time(running.start, planned.end_ms);
        if end.is_some_and(|end| now < end) {
            finished = false;
            end_ms = Some(planned.end_ms);
            planner.wake = earliest(planner.wake, end);
            let mut blends = Vec::new();
            effect_blends(&planned.tracks, &shot.chains, &mut blends);
            for blend in &blends {
                widen(&mut shot, &blend.owner, &blend.from);
            }
            let tracks = planned
                .tracks
                .into_iter()
                .map(|(visual, track)| {
                    (key(&visual.image, &shot), track, visual.seen, visual.motion)
                })
                .collect();
            let to = planner
                .background(&running.to)
                .unwrap_or(Color::TRANSPARENT);
            // What was on screen when a transition was cut short has no background of
            // its own: the shown scene's stands in.
            let from = match &running.from {
                transition::From::Scene(from) => planner.background(from),
                transition::From::OnScreen(_) => Some(to),
            };
            let moves = (
                match &running.from {
                    transition::From::Scene(from) => planner.camera_moves(from, &mut shot),
                    transition::From::OnScreen(_) => None,
                },
                planner.camera_moves(&running.to, &mut shot),
            );
            shot.transition = Some(Transitioning {
                start: running.start,
                tracks,
                backgrounds: (from.unwrap_or(Color::TRANSPARENT), to),
                cameras: planner.track_cameras(running, pairs),
                moves,
                blends,
                end_ms: planned.end_ms,
            });
            shot.passes = schedule_tracks(&shot);
        } else {
            // Ended: plan the program scene alone, without the transition's leftovers.
            shot.scenes.clear();
            shot.frames.clear();
            shot.chains.clear();
            // An item's box goes with its chain: a box left over would be found again
            // without its effects' chain being planned (#960).
            shot.boxes.clear();
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
    for mix in state.video_mixes().filter(|_| mixes) {
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
            .chain(
                transition
                    .tracks
                    .iter()
                    .filter_map(|(key, _, _, _)| match key {
                        GpuKey::Capture(spec) => Some(spec.clone()),
                        _ => None,
                    }),
            )
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
        ChainStep::Pass(pass) => Some(&**pass),
        ChainStep::Run(_) | ChainStep::Box(_) => None,
    }))
    .collect::<Vec<_>>();
    shot.captures.extend(read);
    if mixes {
        shot.stages = plan_stages(state, assets);
    }
    shot.moves = shot.has_motion();
    Planning {
        shot,
        wake: planner.wake,
        finished,
        end_ms,
    }
}

/// The output stage's video chains (ADR-0080): the master stage's over the program and
/// over each video mix, when it has an enabled filter, and each output's own over what
/// the master stage made of the mix it shows ([`shown_mix`]), as its encoder session is
/// keyed (`engine::encode_key`).
fn plan_stages(state: &State, assets: &FilterAssets) -> BTreeMap<ChainOwner, Chain> {
    let master = state.master().chain(FilterChain::Video);
    let mut stages = BTreeMap::new();
    let mixes = std::iter::once(None).chain(state.video_mixes().map(|mix| Some(mix.id().clone())));
    for mix in mixes {
        let owner = ChainOwner::Master(mix.clone());
        if let Some(chain) = Chain::of_filters(&owner, master, GpuKey::Mixed(mix), assets) {
            stages.insert(owner, chain);
        }
    }
    for output in state.outputs() {
        let mix = shown_mix(state, output);
        let master_owner = ChainOwner::Master(mix.clone());
        let input = if stages.contains_key(&master_owner) {
            GpuKey::Filtered(master_owner)
        } else {
            GpuKey::Mixed(mix)
        };
        let owner = ChainOwner::Output(output.id().clone());
        let filters = output.filters().chain(FilterChain::Video);
        if let Some(chain) = Chain::of_filters(&owner, filters, input, assets) {
            stages.insert(owner, chain);
        }
    }
    stages
}

/// The video mix an output's frames are drawn from, as its encoder session is keyed
/// (`engine::encode_key`) and its output stage planned ([`plan_stages`]): its mix if the
/// collection has it, else the program (`None`). A virtual camera always shows the
/// program at the canvas size, whatever mix it names (#1006).
pub(crate) fn shown_mix(state: &State, output: &GraphOutput) -> Option<VideoMixId> {
    if matches!(output.kind(), OutputKind::VirtualCam) {
        return None;
    }
    output
        .video_mix()
        .filter(|id| state.video_mix(id).is_some())
        .cloned()
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
    let (planned, pairs) = planner.transition(running, &mut shot);
    let cameras = planner.track_cameras(running, pairs);
    let elapsed_ms = running.elapsed_ms(now);
    planned.on_screen_seen(elapsed_ms, canvas_size(state.canvas()), |visual, track| {
        Seen::Held {
            camera: cameras.camera(track, visual.seen, elapsed_ms),
            depth_run: cameras.depth_run(track, visual.seen),
        }
    })
}

fn canvas_size(canvas: Canvas) -> [f32; 2] {
    [canvas.width as f32, canvas.height as f32]
}

/// The size a planned scene's picture is: the canvas, through the scene's chain when it
/// has one.
fn scene_size(out: &Shot, scene: &SceneId, canvas: Canvas) -> [u32; 2] {
    let (width, height) = match out.chains.get(&ChainOwner::Scene(scene.clone())) {
        Some(chain) => chain.output_size(canvas.width, canvas.height),
        None => (canvas.width, canvas.height),
    };
    [width, height]
}

/// The size source `id`'s planned chain draws for its frame. `None` when either isn't
/// in the shot.
fn filtered_size(out: &Shot, id: &SourceId) -> Option<[u32; 2]> {
    let chain = out.chains.get(&ChainOwner::Source(id.clone()))?;
    let [width, height] = out.frames.get(id)?.extent;
    let (width, height) = chain.output_size(width, height);
    Some([width, height])
}

/// The size `scene` source `id`'s chain draws over `scene` (ADR-0065 §4): the scene's
/// picture, through the chain.
fn placed_size(out: &Shot, id: &SourceId, scene: &SceneId, canvas: Canvas) -> [u32; 2] {
    let [width, height] = scene_size(out, scene, canvas);
    let owner = ChainOwner::Placed(id.clone(), scene.clone());
    let (width, height) = out
        .chains
        .get(&owner)
        .map_or((width, height), |chain| chain.output_size(width, height));
    [width, height]
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

pub(crate) fn earliest(wake: Option<MediaTime>, at: Option<MediaTime>) -> Option<MediaTime> {
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
        SourceRef::Placed(id, scene) => {
            GpuKey::Filtered(ChainOwner::Placed(id.clone(), scene.clone()))
        }
        SourceRef::Boxed(scene, item) => {
            let owner = ChainOwner::Item(scene.clone(), item.clone());
            // A model's layer with no effects is shown as drawn (ADR-0107).
            if shot.chains.contains_key(&owner) {
                GpuKey::Filtered(owner)
            } else {
                GpuKey::ItemBox(scene.clone(), item.clone())
            }
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
    for (key, _, _, _) in shot.transition.iter().flat_map(|t| &t.tracks) {
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

impl<'a> Planner<'a, '_> {
    /// The scene a planned ID draws, and the keyframe scene whose overrides its items
    /// show: a scene of the collection, a keyframe scene of its own, or a group's
    /// [`view_id`] through a keyframe scene.
    fn drawn(&self, id: &SceneId) -> Option<(&'a GraphScene, Option<&'a GraphScene>)> {
        let state = self.state;
        if let Some(scene) = state.scene(id) {
            return Some((scene, scene.variant_of().is_some().then_some(scene)));
        }
        let (group, keyframe) = view_of(id)?;
        Some((state.scene(&group)?, Some(state.scene(&keyframe)?)))
    }

    /// The items the planned scene `id` draws, bottom first, at their effective values.
    fn items_of(&self, id: &SceneId) -> Vec<std::borrow::Cow<'a, GraphItem>> {
        match self.drawn(id) {
            Some((scene, Some(keyframe))) if scene.variant_of().is_none() => {
                State::items_through(scene, keyframe)
            }
            Some((scene, _)) => self.state.shown_items(scene),
            None => Vec::new(),
        }
    }

    /// The ID to plan scene `scene_id` under, placed by an item of a scene drawn
    /// through `keyframe`: the group's view when the keyframe overrides one of its
    /// children, at any depth (ADR-0040 §2); else its view through the keyframe scene
    /// its placements draw it through, when it has one (keyframe tracks, ADR-0116);
    /// else the scene itself.
    fn nested(&self, keyframe: Option<&GraphScene>, scene_id: &SceneId) -> SceneId {
        let state = self.state;
        if let Some(keyframe) = keyframe {
            let group = state
                .scene(scene_id)
                .is_some_and(|scene| scene.role() == SceneRole::Group);
            let view = view_id(scene_id, keyframe.id());
            if group && state.overrides_within(keyframe, scene_id) && state.scene(&view).is_none() {
                return view;
            }
        }
        match state.placed_keyframe(scene_id) {
            Some(placed) => {
                let view = view_id(scene_id, placed.id());
                if state.scene(&view).is_none() {
                    view
                } else {
                    scene_id.clone()
                }
            }
            None => scene_id.clone(),
        }
    }

    /// The camera the planned scene `id` draws through, if it is a 3D scene (ADR-0094):
    /// its own, or a keyframe scene's parent's where it has no override.
    ///
    /// A keyframe scene always has one, even of a 2D parent, so that a live camera on
    /// the parent reaches it.
    fn camera(&self, id: &SceneId) -> Option<PassCamera> {
        let (scene, _) = self.drawn(id)?;
        let own = scene.camera().copied();
        let parent = scene.variant_of().map(|parent| {
            let committed = self
                .state
                .scene(parent)
                .and_then(|parent| parent.camera().copied());
            (parent.clone(), committed)
        });
        // A follow camera on a scene with no camera of its own supplies the framing
        // camera, so the scene draws as it does in 2D until the pointer moves it
        // (ADR-0120).
        let family = scene.variant_of().unwrap_or(scene.id());
        let supplied = self
            .state
            .scene(family)
            .and_then(|family| family.camera_follow())
            .map(|_| SceneCamera::framing(self.state.canvas()));
        (own.is_some() || parent.is_some() || supplied.is_some()).then_some(PassCamera {
            own,
            parent,
            supplied,
            moves: None,
        })
    }

    /// The camera motion and follow camera of the planned scene `id`'s family
    /// (ADR-0113), `None` when it has neither. The followed item is the one the scene
    /// draws, at its effective geometry, and its capture the one the shot places for it.
    fn camera_moves(&mut self, id: &SceneId, out: &mut Shot) -> Option<Arc<CameraMoves>> {
        let state = self.state;
        let (scene, _) = self.drawn(id)?;
        let family = scene.variant_of().unwrap_or(scene.id()).clone();
        let parent = state.scene(&family)?;
        let motion = CameraMotion::of(parent.camera_motion());
        let follow = parent.camera_follow();
        if motion.is_still() && follow.is_none() {
            return None;
        }
        let follow = follow.and_then(|follow| {
            let item = self
                .items_of(id)
                .into_iter()
                .find(|item| item.id() == &follow.item_id)?;
            let GraphPlacement::Geometry(geometry) = item.placement() else {
                return None;
            };
            let c = item.crop();
            let capture = state.source(item.source_id()).and_then(|source| {
                match self.shown(source, out)? {
                    SourceRef::Capture(spec) => Some(spec),
                    SourceRef::Filtered(source) => {
                        match &out.chains.get(&ChainOwner::Source(source))?.input {
                            GpuKey::Capture(spec) => Some(spec.clone()),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            });
            Some(FollowPlan::new(
                follow,
                geometry,
                [c.left, c.top, c.right, c.bottom],
                capture,
            ))
        });
        Some(Arc::new(CameraMoves {
            family,
            motion,
            follow,
        }))
    }

    /// The cameras `running`'s tracks are seen through (ADR-0094 decision 7).
    /// The cameras `running`'s tracks are seen through. `pairs` is whether it moves
    /// ([`Self::transition`]'s answer): a Move between a 2D side and a 3D one sees the 2D
    /// side through the framing camera, which draws it as 2D does (ADR-0128).
    fn track_cameras(&self, running: &Running, pairs: bool) -> TrackCameras {
        let committed = |id: &SceneId| self.camera(id).and_then(|camera| camera.committed());
        let ends = self.move_ends(running);
        let ends = match ends {
            (None, Some(_)) | (Some(_), None) if pairs => {
                let framing = SceneCamera::framing(self.state.canvas());
                (ends.0.or(Some(framing)), ends.1.or(Some(framing)))
            }
            ends => ends,
        };
        TrackCameras {
            ends,
            overlay: running
                .run
                .stinger
                .as_ref()
                .and_then(|stinger| stinger.scene.as_ref())
                .and_then(committed),
            timing: Timing {
                delay_ms: 0.0,
                duration_ms: running.run.duration_ms as f32,
                easing: running.run.easing,
            },
            pairs,
            overlay_run: match &running.from {
                From::OnScreen(visuals) => visuals
                    .iter()
                    .filter_map(|visual| match visual.seen {
                        Seen::Held { depth_run, .. } => Some(depth_run),
                        Seen::AsPlanned => None,
                    })
                    .max()
                    .map_or(1, |run| run.saturating_add(1)),
                From::Scene(_) => 1,
            },
        }
    }

    /// The background the planned scene `id` draws under its items, if it has one: a
    /// keyframe scene shows its parent's, and a group none, since it is drawn in the
    /// scene that holds it (ADR-0089 §4).
    fn background(&self, id: &SceneId) -> Option<Color> {
        let (scene, _) = self.drawn(id)?;
        if scene.role() == SceneRole::Group {
            return None;
        }
        let scene = scene
            .variant_of()
            .and_then(|parent| self.state.scene(parent))
            .unwrap_or(scene);
        scene.options().background_rgba().map(Color::from_rgba8)
    }

    /// The keyframe scene whose overrides the planned scene `id` shows, if any.
    fn keyframe_of(&self, id: &SceneId) -> Option<&'a GraphScene> {
        self.drawn(id).and_then(|(_, keyframe)| keyframe)
    }

    /// Plans `running`: what it starts from, paired with the program scene's items. The
    /// frames and nested scenes either side shows go into `out`.
    ///
    /// Within a family, a group that moves as one item carries its children's tracks
    /// inside its layer, from their values on one side to those on the other
    /// (ADR-0049 §3), and is drawn from a scene of the shot that holds them.
    /// The committed cameras `running` moves between: what was on screen's, and the
    /// shown scene's, `None` for a 2D side.
    fn move_ends(&self, running: &Running) -> (Option<SceneCamera>, Option<SceneCamera>) {
        let committed = self
            .camera(&running.to)
            .and_then(|camera| camera.committed());
        (running.from_camera, committed)
    }

    /// Whether every one of `visuals`, the 2D side of a Move into or out of 3D, draws the
    /// same through the framing camera as in 2D (ADR-0128): flat, with no tilt, turn or
    /// depth. A tilted item's own perspective projects about its own centre, the camera
    /// about the canvas's, so even at the framing camera's focal length an item off the
    /// centre would jump.
    fn framed_alike(visuals: &[Visual]) -> bool {
        visuals.iter().all(|visual| {
            let g = &visual.at.geometry;
            g.tilt_deg.get() == 0.0 && g.turn_deg.get() == 0.0 && g.depth_px.get() == 0.0
        })
    }

    fn transition(&mut self, running: &Running, out: &mut Shot) -> (Planned, bool) {
        let flatten = running.run.pairs;
        let mut from: Vec<Visual> = match &running.from {
            From::Scene(scene) => self.endpoint(scene, flatten, out),
            // A group whose children were moving is drawn once its track is known.
            From::OnScreen(items) => items
                .iter()
                .filter(|visual| visual.inside.is_some() || self.shows(&visual.image, out))
                .cloned()
                .collect(),
        };
        let mut to = self.endpoint(&running.to, flatten, out);
        // A Move between 2D and 3D sees the 2D side through the framing camera
        // (ADR-0128); where that side can't be drawn the same through it, it fades.
        let mut pairs = running.run.pairs;
        let faded;
        let mut run = &running.run;
        if pairs {
            let two_d = match self.move_ends(running) {
                (None, Some(_)) => Some(&from),
                (Some(_), None) => Some(&to),
                _ => None,
            };
            if two_d.is_some_and(|side| !Self::framed_alike(side)) {
                pairs = false;
                // A fade draws each side whole, as a Fade preset would: nested scenes
                // stay one picture.
                if let From::Scene(scene) = &running.from {
                    from = self.endpoint(scene, false, out);
                }
                to = self.endpoint(&running.to, false, out);
                faded = transition::Run {
                    pairs: false,
                    ..running.run.clone()
                };
                run = &faded;
            }
        }
        // A keyframe's timing applies to switches into it (ADR-0129).
        let into = self
            .keyframe_of(&running.to)
            .map(|keyframe| keyframe.overrides());
        let (mut planned, partners) = transition::plan(&from, &to, run, running.by_item, into);
        if running.by_item && pairs {
            self.move_inside(&mut planned, &from, &partners, run, out);
        }
        // Each group with children inside it gets a scene of the shot that draws them.
        let tracks = std::mem::take(&mut planned.tracks);
        let mut redrawn = false;
        for (index, (visual, track)) in tracks.into_iter().enumerate() {
            if visual.inside.is_none() {
                planned.tracks.push((visual, track));
                continue;
            }
            redrawn = true;
            match self.reshown(&visual, &inner_id(index), running.start, out) {
                Some(visual) => planned.tracks.push((visual, track)),
                // A group arriving is drawn as it is: its children show the new side's
                // values.
                None if partners[index].is_some() => {
                    let shown = Visual {
                        inside: None,
                        ..visual
                    };
                    if self.shows(&shown.image, out) {
                        planned.tracks.push((shown, track));
                    }
                }
                // A group leaving that can't be drawn any more is left out.
                None => {}
            }
        }
        // The layers either side planned each such group with, their chains and its
        // item's effects are no longer shown: they no longer run each frame, unless
        // another track reads them.
        if redrawn {
            out.keep_read(planned.tracks.iter().map(|(visual, _)| &visual.image));
        }
        // A stinger's scene plays over all of it, its items drawn as they are placed.
        if let Some(stinger) = &running.run.stinger {
            let cut_ms = stinger.cut_ms as f32;
            if let Some(scene) = stinger.scene.as_ref().filter(|id| self.drawn(id).is_some()) {
                let overlay = self.endpoint(scene, false, out);
                planned
                    .tracks
                    .extend(transition::through(&overlay, &running.run, cut_ms));
            }
            planned.end_ms = planned.end_ms.max(running.run.duration_ms as f32);
        }
        (planned, pairs)
    }

    /// Gives each pair of `planned` that moves a group as one image, between two views
    /// of it through members of a family, its children's tracks inside the group's
    /// layer: planned by item from their values on the `from` side to the new side's,
    /// with the group item's progress, and the same again for a group among them
    /// (ADR-0049 §3). `partners` is each track's partner in `from`.
    fn move_inside(
        &mut self,
        planned: &mut Planned,
        from: &[Visual],
        partners: &[Option<usize>],
        run: &transition::Run,
        out: &mut Shot,
    ) {
        for ((visual, track), partner) in planned.tracks.iter_mut().zip(partners) {
            let (Some(partner), Track::Pair { timing, .. }) = (partner, *track) else {
                continue;
            };
            let outgoing = &from[*partner];
            let Some(to_layer) = self.group_layer(&visual.image, out) else {
                continue;
            };
            let Some((group, _)) = self.drawn(&to_layer) else {
                continue;
            };
            let children = match &outgoing.inside {
                Some(inside) if inside.group == *group.id() => inside
                    .planned
                    .tracks
                    .iter()
                    .map(|(child, _)| child.clone())
                    .collect(),
                Some(_) => continue,
                None => {
                    let Some(from_layer) = self.group_layer(&outgoing.image, out) else {
                        continue;
                    };
                    let same = self
                        .drawn(&from_layer)
                        .is_some_and(|(scene, _)| scene.id() == group.id());
                    if !same || from_layer == to_layer {
                        continue;
                    }
                    self.visuals(&from_layer, false, out)
                }
            };
            let arriving = self.visuals(&to_layer, false, out);
            // The children follow the group item's progress, whatever their own timing.
            let (mut inner, inner_partners) =
                transition::plan(&children, &arriving, run, true, None);
            for (_, track) in &mut inner.tracks {
                *track = transition::with_timing(*track, timing);
            }
            inner.end_ms = timing.end_ms();
            self.move_inside(&mut inner, &children, &inner_partners, run, out);
            visual.inside = Some(Arc::new(transition::Inside {
                group: group.id().clone(),
                boxed_in: match &visual.image {
                    SourceRef::Boxed(scene, _) => Some(scene.clone()),
                    _ => None,
                },
                planned: inner,
            }));
        }
    }

    /// The scene the layer of `image` draws, for a nested scene placed as one image:
    /// the scene itself, or the one its item's effects read.
    fn group_layer(&self, image: &SourceRef, out: &Shot) -> Option<SceneId> {
        match image {
            SourceRef::Scene(id) => Some(id.clone()),
            SourceRef::Boxed(scene, item) => {
                match &out
                    .boxes
                    .get(&ChainOwner::Item(scene.clone(), item.clone()))?
                    .source
                {
                    SourceRef::Scene(id) => Some(id.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// `visual` as the shot can draw it, or `None` when it can't. A group whose children
    /// move inside its layer ([`transition::Inside`]) is drawn from scene `id` of the
    /// shot, which places each child by its track, through the group's video chain and
    /// the group item's effects; a group among its children the same way, below `id`.
    fn reshown(
        &mut self,
        visual: &Visual,
        id: &SceneId,
        start: MediaTime,
        out: &mut Shot,
    ) -> Option<Visual> {
        let Some(inside) = visual.inside.as_deref() else {
            return self.shows(&visual.image, out).then(|| visual.clone());
        };
        let state = self.state;
        if state.scene(id).is_some() {
            // A scene of the collection has the ID: the group shows as it is.
            return None;
        }
        let mut placed = Vec::new();
        let mut tracks = Vec::new();
        for (index, (child, track)) in inside.planned.tracks.iter().enumerate() {
            let Some(child) = self.reshown(child, &nested_inner_id(id, index), start, out) else {
                continue;
            };
            placed.push(Placed {
                item: child.item_id.clone(),
                through: None,
                source: child.image.clone(),
                placement: Placement::Geometry(child.at.geometry),
                crop: child.at.crop,
                opacity: child.at.opacity,
                blend: child.at.blend,
                effects: None,
                moving: Some((start, *track)),
                motion: child.motion.clone(),
            });
            tracks.push((child, *track));
        }
        out.scenes.insert(id.clone(), placed);
        if let Some(group) = state.scene(&inside.group)
            && let Some(chain) = Chain::of_filters(
                &ChainOwner::Scene(id.clone()),
                scene_filters(state, group),
                GpuKey::Composite(id.clone()),
                self.assets,
            )
        {
            self.add_chain(ChainOwner::Scene(id.clone()), chain, out);
        }
        let layer = SourceRef::Scene(id.clone());
        let image = match &inside.boxed_in {
            None => layer,
            Some(scene) => {
                let item = self
                    .items_of(scene)
                    .into_iter()
                    .find(|item| item.as_ref().id() == &visual.item_id)?;
                self.boxed(id, item.as_ref(), &layer, out)?;
                SourceRef::Boxed(id.clone(), visual.item_id.clone())
            }
        };
        Some(Visual {
            image,
            inside: Some(Arc::new(transition::Inside {
                planned: Planned {
                    tracks,
                    end_ms: inside.planned.end_ms,
                },
                ..inside.clone()
            })),
            ..visual.clone()
        })
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
                blend: BlendMode::Normal,
            },
            size: Some([canvas.width, canvas.height]),
            // One picture that pairs with nothing crossfades (ADR-0065 §6), whatever
            // the preset's `unmatched` effect.
            settings: ItemTransition {
                glide: Glide::Never,
                appear: jstream_protocol::TransitionEffect::Fade,
                disappear: jstream_protocol::TransitionEffect::Fade,
                ..ItemTransition::default()
            },
            effects: Vec::new(),
            blend_from: None,
            inside: None,
            seen: Seen::AsPlanned,
            motion: TrackMotion::same(ItemId::new(""), Arc::from([])),
        }]
    }

    /// The video chain of scene `id` as the collection holds it, over its composite: a
    /// keyframe scene's is its parent's (ADR-0024). `None` when no filter of it runs.
    fn scene_chain(&self, id: &SceneId) -> Option<Chain> {
        let (scene, _) = self.drawn(id)?;
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
            // Over the ID the scene was planned under, a group's view included.
            SourceRef::Placed(id, scene) => {
                let state = self.state;
                state.source(id).is_some_and(|source| {
                    matches!(
                        self.scened(source, scene.clone(), out, &mut BTreeSet::new()),
                        SourceRef::Placed(..)
                    )
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
        let keyframe = self.keyframe_of(scene_id);
        let Some(item) = self
            .items_of(scene_id)
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
                let nested = self.nested(keyframe, scene_id);
                self.scened(source, nested, out, &mut BTreeSet::new())
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
        let mut chain = Chain::of_filters(
            &owner,
            item.effects(),
            GpuKey::ItemBox(scene.clone(), item.id().clone()),
            self.assets,
        );
        // A model is drawn into a layer of its own, effects or not (ADR-0107).
        let model = matches!(source, SourceRef::Frame(id)
            if out.frames.get(id).is_some_and(|frame| frame.model.is_some()));
        if chain.is_none() && !model {
            return None;
        }
        let canvas = self.state.canvas();
        let input = match source {
            SourceRef::Frame(id) => out.frames.get(id).map(|frame| frame.extent),
            SourceRef::Filtered(id) => filtered_size(out, id),
            SourceRef::Placed(id, scene) => Some(placed_size(out, id, scene, canvas)),
            SourceRef::Scene(id) => Some(scene_size(out, id, canvas)),
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
        let boxed = match (&chain, item.effects_space()) {
            _ if model => Boxed {
                model: true,
                ..Boxed::over_canvas(canvas, input)?
            },
            (_, jstream_protocol::EffectsSpace::Canvas) => Boxed::over_canvas(canvas, input)?,
            (Some(chain), _) => Boxed::new(&geometry, chain.outset(), input)?,
            (None, _) => return None,
        };
        let camera = match model {
            true => self.camera(scene).map(|mut camera| {
                camera.moves = self.camera_moves(scene, out);
                camera
            }),
            false => None,
        };
        if let Some(chain) = chain.as_mut() {
            chain.pad = (!boxed.canvas).then(|| chain.pad_at(boxed.scale)).flatten();
        }
        out.boxes.insert(
            owner.clone(),
            ItemBox {
                item: item.id().clone(),
                through: self
                    .keyframe_of(scene)
                    .and_then(|keyframe| Through::of(keyframe, item.id())),
                source: source.clone(),
                placement: Placement::Geometry(geometry),
                crop: crop(item.crop()),
                boxed,
                motion: motion::of(item),
                camera,
            },
        );
        out.audio
            .record(item.id(), item.motion(), self.assets.motion_heard());
        if let Some(chain) = chain {
            self.add_chain(owner, chain, out);
        }
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
        if self.drawn(id).is_none() || !path.insert(id.clone()) {
            return;
        }
        let keyframe = self.keyframe_of(id);
        let canvas = state.canvas();
        for item in self.items_of(id) {
            let item = item.as_ref();
            if !item.visible() {
                continue;
            }
            let Some(source) = state.source(item.source_id()) else {
                continue;
            };
            let (image, size) = match source.kind() {
                SourceKind::Scene { scene_id } => {
                    // A group's children a keyframe overrides are drawn, and paired,
                    // through it (ADR-0049 §3).
                    let scene_id = &self.nested(keyframe, scene_id);
                    // A scene with a chain is one picture (ADR-0065 §6), and so is one
                    // placed with a blend mode or holding an item that has one: its
                    // items blend with its own layer, not the scene it is placed in
                    // (ADR-0082).
                    if flatten
                        && depth + 1 < MAX_NESTING
                        && covers(item, canvas)
                        && item.opacity().get() >= OPAQUE
                        && item.blend().is_normal()
                        && self
                            .items_of(scene_id)
                            .iter()
                            .all(|inner| inner.blend().is_normal())
                        && self.scene_chain(scene_id).is_none()
                        && !self.filters_scene(source)
                        && !item.effects().iter().any(|effect| {
                            effect.enabled()
                                && effect.kind() != jstream_protocol::FOLLOW_POINTER_KIND
                        })
                        // A group that moves on its own is one picture (ADR-0099).
                        && item.motion().is_empty()
                    {
                        self.visuals_into(scene_id, flatten, depth + 1, path, out, visuals);
                        continue;
                    }
                    match self.scened(source, scene_id.clone(), out, &mut BTreeSet::new()) {
                        SourceRef::Placed(id, scene) => {
                            let size = placed_size(out, &id, &scene, canvas);
                            (SourceRef::Placed(id, scene), Some(size))
                        }
                        image => (image, Some([canvas.width, canvas.height])),
                    }
                }
                _ => match self.shown(source, out) {
                    Some(SourceRef::Frame(id)) => {
                        let size = out.frames.get(&id).map(|frame| frame.extent);
                        (SourceRef::Frame(id), size)
                    }
                    // A filtered frame is the size the chain draws for it.
                    Some(SourceRef::Filtered(id)) => {
                        let size = filtered_size(out, &id);
                        (SourceRef::Filtered(id), size)
                    }
                    // A capture's size isn't known when planning: the canvas stands in.
                    Some(capture @ SourceRef::Capture(_)) => (capture, None),
                    Some(SourceRef::Scene(_) | SourceRef::Placed(..) | SourceRef::Boxed(..))
                    | None => continue,
                },
            };
            let Some(geometry) = endpoint_geometry(item, size, canvas) else {
                continue;
            };
            // An item with effects is its effects' layer over its grown box, uncropped.
            let boxed = self.boxed(id, item, &image, out);
            // An item whose effects run over the canvas covers the canvas on both sides
            // of a switch, so it can't glide (ADR-0106): it disappears and appears.
            let mut settings = *item.transition();
            if boxed.is_some_and(|boxed| boxed.canvas) {
                settings.glide = jstream_protocol::Glide::Never;
            }
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
                    blend: item.blend(),
                },
                size,
                settings,
                effects,
                blend_from: None,
                inside: None,
                seen: Seen::AsPlanned,
                motion: TrackMotion::same(item.id().clone(), motion::of(item)),
            });
            out.audio
                .record(item.id(), item.motion(), self.assets.motion_heard());
        }
        path.remove(id);
    }

    fn collect(&mut self, id: &SceneId, out: &mut Shot, path: &mut BTreeSet<SceneId>) {
        if out.scenes.contains_key(id) || !path.insert(id.clone()) {
            return;
        }
        let state = self.state;
        let mut placed = Vec::new();
        if self.drawn(id).is_some() {
            // A keyframe scene draws its parent's items at their effective values, and
            // a group's view its children at theirs.
            let keyframe = self.keyframe_of(id);
            for item in self.items_of(id) {
                let item = item.as_ref();
                if !item.visible() {
                    continue;
                }
                let Some(source) = state.source(item.source_id()) else {
                    continue;
                };
                let source = match source.kind() {
                    SourceKind::Scene { scene_id } => {
                        let nested = self.nested(keyframe, scene_id);
                        self.scened(source, nested, out, path)
                    }
                    _ => match self.shown(source, out) {
                        Some(source) => source,
                        None => continue,
                    },
                };
                let effects = self.boxed(id, item, &source, out);
                placed.push(Placed {
                    item: item.id().clone(),
                    through: keyframe.and_then(|keyframe| Through::of(keyframe, item.id())),
                    source,
                    placement: placement(item.placement()),
                    crop: crop(item.crop()),
                    opacity: item.opacity().get(),
                    blend: item.blend(),
                    effects,
                    moving: None,
                    motion: TrackMotion::same(item.id().clone(), motion::of(item)),
                });
                out.audio
                    .record(item.id(), item.motion(), self.assets.motion_heard());
            }
        }
        out.scenes.insert(id.clone(), placed);
        if let Some(background) = self.background(id) {
            out.backgrounds.insert(id.clone(), background);
        }
        if let Some(mut camera) = self.camera(id) {
            camera.moves = self.camera_moves(id, out);
            out.cameras.insert(id.clone(), camera);
        }
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

    /// What `source`, a `scene` source, shows of scene `nested`, the ID its scene is
    /// planned under there (the scene, or a group's view through a keyframe): the
    /// nested scene, collected into `out`, then the source's own video chain when a
    /// filter of it runs, whose output is placed like any source's frame (ADR-0065 §4).
    /// The chain ([`ChainOwner::Placed`]) reads the scene's layer, or the scene's own
    /// chain's output when it has one, and goes in `out` once per shot for each ID.
    fn scened(
        &mut self,
        source: &Source,
        nested: SceneId,
        out: &mut Shot,
        path: &mut BTreeSet<SceneId>,
    ) -> SourceRef {
        self.collect(&nested, out, path);
        let owner = ChainOwner::Placed(source.id().clone(), nested.clone());
        if out.chains.contains_key(&owner) {
            return SourceRef::Placed(source.id().clone(), nested);
        }
        let input = key(&SourceRef::Scene(nested.clone()), out);
        let filters = source.filters().chain(FilterChain::Video);
        match Chain::of_filters(&owner, filters, input, self.assets) {
            Some(chain) => {
                self.add_chain(owner, chain, out);
                SourceRef::Placed(source.id().clone(), nested)
            }
            None => SourceRef::Scene(nested),
        }
    }

    /// Whether a filter of `source`'s video chain runs, so a `scene` source shows its
    /// scene as one filtered picture.
    fn filters_scene(&self, source: &Source) -> bool {
        // A scene source has no program, so the shown version is never read.
        Chain::of(source, GpuKey::Program, 0, self.assets).is_some()
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
        // A shader source's program restarts when its stand-in shows a new frame.
        let shown = out.frames.get(id).map_or(0, |frame| frame.version);
        match Chain::of(source, key(&image, out), shown, self.assets) {
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
    /// A mix an output draws (`None` for the program of an output that names none), at
    /// the canvas size, which the output stage's chains run over (ADR-0080).
    Mixed(Option<VideoMixId>),
}

/// The depth a scene that a video filter reads (`scene_mask`) is drawn at: as a scene
/// nested one level down, so what it nests counts toward [`MAX_NESTING`] the same way.
const REFERENCE_DEPTH: usize = 1;

/// The depth a scene with a video chain (ADR-0065) is composited at for its chain: the
/// program scene's. Its composite is drawn once per frame, however deep it is placed,
/// so its own nested scenes count toward [`MAX_NESTING`] from it, as a program scene's
/// do, and a filtered program scene draws as deep as an unfiltered one.
const COMPOSITE_DEPTH: usize = 0;

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
    /// A scene a `scene_mask` reads, or a scene that one nests. Boxed: a pass holds two
    /// cameras, which would make every step that size.
    Pass(Box<Pass>),
    /// An item with effects, drawn into its [`GpuKey::ItemBox`] before they run.
    Box(ChainOwner),
    /// The video chain of a source or a scene.
    Run(ChainOwner),
}

/// What a shot's frame slots held at one moment, by source ([`Shot::pin`]).
#[derive(Debug, Default)]
pub(crate) struct Pinned(BTreeMap<SourceId, Arc<crate::slot::Latest>>);

/// What [`compose`] draws: which mix, and whether as an output gets it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct View<'a> {
    /// The mix to draw; `None` for the program.
    pub(crate) mix: Option<&'a VideoMixId>,
    /// Whether it is drawn for an output, through the master stage's video chain and
    /// `owner`'s (ADR-0080). The editing canvas, previews and `video.snapshot` show the
    /// mix before the master stage.
    pub(crate) stage: bool,
    /// The output whose own video chain the frame runs through, for a frame drawn for
    /// that output alone.
    pub(crate) owner: Option<&'a OutputId>,
    /// What the shot's frame slots held when a frame sheet's picture was taken, drawn in
    /// place of what they hold now ([`Shot::pin`]). `None` draws what they hold now.
    pub(crate) pinned: Option<&'a Pinned>,
}

impl<'a> View<'a> {
    /// `mix` as the outputs drawing it get it, through `owner`'s own chain if any.
    pub(crate) fn output(mix: Option<&'a VideoMixId>, owner: Option<&'a OutputId>) -> Self {
        Self {
            mix,
            stage: true,
            owner,
            pinned: None,
        }
    }

    /// `mix` before the master stage.
    #[cfg(test)]
    pub(crate) fn mix(mix: Option<&'a VideoMixId>) -> Self {
        Self {
            mix,
            ..Self::default()
        }
    }
}

/// The output stage a frame goes through ([`Shot::stage_of`]).
struct Stage<'s> {
    /// The mix's layer at the canvas size, which the first chain reads.
    mixed: GpuKey,
    master: Option<(&'s ChainOwner, &'s Chain)>,
    output: Option<(&'s ChainOwner, &'s Chain)>,
}

/// One composite in a frame: a scene at a nesting depth (the program scene is 0).
#[derive(Debug, PartialEq)]
struct Pass {
    scene: SceneId,
    depth: usize,
    /// Where the pass draws: its [`GpuKey::Layer`]. The last pass, the program scene,
    /// draws to the output instead.
    key: GpuKey,
    /// What the pass clears to, under its layers: the scene's background, or
    /// transparent.
    background: Color,
    /// The camera its scene draws through (ADR-0094), `None` for a 2D scene.
    camera: Option<PassCamera>,
    layers: Vec<Layer>,
}

/// The committed cameras a pass resolves its camera from (ADR-0094 §6), so that live
/// values resolve the same way `State::effective_camera` does.
#[derive(Clone, Debug, PartialEq)]
struct PassCamera {
    /// The scene's own camera, or a keyframe scene's override.
    own: Option<SceneCamera>,
    /// For a keyframe scene, its parent and the parent's committed camera.
    parent: Option<(SceneId, Option<SceneCamera>)>,
    /// The framing camera a follow camera supplies (ADR-0120), drawn through when the
    /// scene would otherwise be 2D.
    supplied: Option<SceneCamera>,
    /// Its family's camera motion and follow camera (ADR-0113), when it has either.
    moves: Option<Arc<CameraMoves>>,
}

impl PassCamera {
    /// The camera it draws through with live cameras applied: `live` gives the live
    /// value set on a scene, `Some(None)` for a live removal. A keyframe scene draws its
    /// override, else its parent's camera; a keyframe of a 2D parent may have its own
    /// (ADR-0128). A live removal of a parent's committed camera makes every keyframe 2D,
    /// overrides too, as committing it would drop them (ADR-0094 §6).
    fn resolve(
        &self,
        scene: &SceneId,
        live: impl Fn(&SceneId) -> Option<Option<SceneCamera>>,
    ) -> Option<SceneCamera> {
        let own = live(scene).unwrap_or(self.own);
        let drawn = match &self.parent {
            None => own,
            Some((parent, committed)) => {
                let parent = live(parent).unwrap_or(*committed);
                if parent.is_none() && committed.is_some() {
                    None
                } else {
                    own.or(parent)
                }
            }
        };
        drawn.or(self.supplied)
    }

    /// The camera it draws through with no live values.
    fn committed(&self) -> Option<SceneCamera> {
        match &self.parent {
            None => self.own,
            Some((_, parent)) => self.own.or(*parent),
        }
        .or(self.supplied)
    }
}

/// One item of a [`Pass`].
#[derive(Debug, PartialEq)]
struct Layer {
    /// The item it draws, whose live values override `placement` and `crop`.
    item: ItemId,
    /// The keyframe scene that draws it, if its pass is one: which live values apply.
    through: Option<Through>,
    /// The cached image the item shows: a frame's upload, the capture's, or the layer an
    /// earlier pass drew.
    image: GpuKey,
    placement: Placement,
    crop: Crop,
    opacity: f32,
    blend: BlendMode,
    /// The box of an item whose effects run: `image` is their output, placed over the
    /// box grown by their outset, uncropped.
    boxed: Option<Boxed>,
    /// Its track, for a group's child a transition moves inside the group's layer.
    moving: Option<Moving>,
    /// Its motion modifiers (ADR-0099): the moving child's both sides, else its own.
    motion: TrackMotion,
}

impl Shot {
    /// Keeps only the scenes, video chains and item boxes that `roots` read, at any
    /// depth: what a nested scene places, the boxes of its items with effects, a
    /// scene's or source's chain, the scenes its filters read and what a box reads. A
    /// scene goes with its chain, so planning it again plans both.
    fn keep_read<'r>(&mut self, roots: impl Iterator<Item = &'r SourceRef>) {
        let mut scenes = BTreeSet::new();
        let mut owners = BTreeSet::new();
        let mut stack: Vec<SourceRef> = roots.cloned().collect();
        while let Some(image) = stack.pop() {
            let owner = match image {
                SourceRef::Frame(_) | SourceRef::Capture(_) => continue,
                SourceRef::Filtered(id) => ChainOwner::Source(id),
                SourceRef::Placed(id, scene) => ChainOwner::Placed(id, scene),
                SourceRef::Boxed(scene, item) => ChainOwner::Item(scene, item),
                SourceRef::Scene(id) => {
                    if !scenes.insert(id.clone()) {
                        continue;
                    }
                    for placed in self.scenes.get(&id).into_iter().flatten() {
                        stack.push(placed.source.clone());
                        if placed.effects.is_some() {
                            stack.push(SourceRef::Boxed(id.clone(), placed.item.clone()));
                        }
                    }
                    ChainOwner::Scene(id)
                }
            };
            if !owners.insert(owner.clone()) {
                continue;
            }
            if let Some(planned) = self.boxes.get(&owner) {
                stack.push(planned.source.clone());
            }
            if let Some(chain) = self.chains.get(&owner) {
                stack.extend(chain.scenes().cloned().map(SourceRef::Scene));
                stack.extend(chain.input_scene().cloned().map(SourceRef::Scene));
            }
        }
        self.scenes.retain(|id, _| scenes.contains(id));
        self.chains.retain(|owner, _| owners.contains(owner));
        self.boxes.retain(|owner, _| owners.contains(owner));
    }

    /// Whether only items draw `frame`, uploaded under `key`: no video chain reads it
    /// (a chain's filters work in the frame's own texels) and it stands in for no model.
    /// Such an upload can be held at the size its items are drawn
    /// ([`ResourceCache::upload_drawn`], #1139).
    fn drawn_only(&self, key: &GpuKey, frame: &Frame) -> bool {
        frame.model.is_none() && !self.chains.values().any(|chain| chain.input == *key)
    }

    /// How much each source whose frame a layer draws is enlarged where it is shown
    /// most, in canvas pixels per pixel of the frame's extent: what its source should
    /// draw at to stay sharp (ADR-0073). It is measured on every pass, the program's and
    /// the video mixes', from the placements the shot holds, not a gesture's live
    /// values, so a drag is measured once it commits. A source a video chain runs over
    /// counts as 1, since its filters' settings are in the frame's pixels. A source the
    /// shot draws only in a transition's tracks, or not at all, isn't listed. A legacy
    /// item draws nothing, and is left out, when
    /// [`legacy_quad`](jstream_protocol::legacy_quad) gives it no quad (#1168).
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
///
/// A legacy item draws nothing when [`legacy_quad`](jstream_protocol::legacy_quad) gives
/// no quad, the same rule `place::legacy` uses: the crop or a zero scale leaves nothing,
/// a field isn't finite, or a finite scale's size or a corner overflows `f32` (#1168).
/// The scale of an item that does draw is still `max(|scale.x|, |scale.y|)`.
fn magnification(placement: Placement, crop: Crop, extent: [u32; 2]) -> f32 {
    let scale = match placement {
        Placement::Legacy(transform) => legacy_magnification(transform, crop, extent),
        // Drawn at its own size.
        Placement::Layer(_) => 1.0,
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

/// The legacy scale a drawn quad is enlarged by, or 0 when [`legacy_quad`] gives nothing.
///
/// [`legacy_quad`]: jstream_protocol::legacy_quad
fn legacy_magnification(transform: LegacyTransform, crop: Crop, extent: [u32; 2]) -> f32 {
    let Some(position) = jstream_protocol::Vec2::new(transform.position[0], transform.position[1])
    else {
        return 0.0;
    };
    let Some(scale) = jstream_protocol::Vec2::new(transform.scale[0], transform.scale[1]) else {
        return 0.0;
    };
    let Some(rotation_deg) = jstream_protocol::Finite32::new(transform.rotation_deg) else {
        return 0.0;
    };
    let transform = jstream_protocol::ItemTransform {
        position,
        scale,
        rotation_deg,
    };
    let cut = ProtocolCrop {
        left: crop.left,
        top: crop.top,
        right: crop.right,
        bottom: crop.bottom,
    };
    match jstream_protocol::legacy_quad(transform, cut, extent[0], extent[1]) {
        Ok(Some(_)) => scale.x.get().abs().max(scale.y.get().abs()),
        _ => 0.0,
    }
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
                through: None,
                image: GpuKey::Filtered(owner),
                placement: Placement::Geometry(geometry),
                crop: Crop::NONE,
                opacity: OPAQUE,
                blend: BlendMode::Normal,
                boxed: None,
                moving: None,
                motion: TrackMotion::same(ItemId::new(""), Arc::from([])),
            })
            .into_iter()
            .collect();
        return vec![Pass {
            scene: program.clone(),
            depth: 0,
            key: GpuKey::Layer(program.clone(), 0),
            // The chain's output has the background in it already, and is flat.
            background: Color::TRANSPARENT,
            camera: None,
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
/// [`COMPOSITE_DEPTH`] into [`GpuKey::Composite`]. A scene a chain's filter reads
/// (`scene_mask`) is drawn at [`REFERENCE_DEPTH`] into [`GpuKey::reference`], or read
/// from its own chain's output when it has one. The graph refuses a cycle through a
/// filter's scene reference (ADR-0026); were one there, the chain that closes it would
/// read its owner's output from the frame before.
fn schedule_chains(shot: &Shot) -> Vec<ChainStep> {
    let mut order = ChainOrder::default();
    // A model's layer without effects reads nothing a chain draws, so it is drawn
    // first, before any pass that shows it (ADR-0107).
    for owner in shot.boxes.keys() {
        if !shot.chains.contains_key(owner) {
            order.steps.push(ChainStep::Box(owner.clone()));
        }
    }
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
                    SourceRef::Placed(source, scene) => {
                        let read = ChainOwner::Placed(source.clone(), scene.clone());
                        self.chain(shot, &read, done);
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
            // A `scene` source's chain reads its scene's layer, or the scene's own
            // chain's output (ADR-0065 §4).
            match &chain.input {
                GpuKey::Layer(scene, depth) => {
                    visit(shot, scene, *depth, &mut BTreeSet::new(), done, self);
                }
                GpuKey::Filtered(read) => self.chain(shot, read, done),
                _ => {}
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
                    (scene, COMPOSITE_DEPTH, GpuKey::Composite(scene.clone())),
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
        self.steps.push(ChainStep::Pass(Box::new(pass)));
    }

    fn filtered(&mut self, shot: &Shot, owner: &ChainOwner, done: &mut BTreeSet<GpuKey>) {
        self.chain(shot, owner, done);
    }
}

/// What an item with a layer of its own shows (ADR-0065 §5): its effects' output, once
/// they are scheduled, or for a model without effects (ADR-0107) the layer as drawn.
fn boxed_key(
    shot: &Shot,
    owner: ChainOwner,
    done: &mut BTreeSet<GpuKey>,
    passes: &mut impl Passes,
) -> GpuKey {
    if shot.chains.contains_key(&owner) {
        passes.filtered(shot, &owner, done);
        GpuKey::Filtered(owner)
    } else {
        match owner {
            ChainOwner::Item(scene, item) => GpuKey::ItemBox(scene, item),
            other => GpuKey::Filtered(other),
        }
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
            // A model's layer without effects is shown as drawn (ADR-0107).
            _ if placed.effects.is_some() => boxed_key(
                shot,
                ChainOwner::Item(id.clone(), placed.item.clone()),
                done,
                passes,
            ),
            // A moving child with effects shows their output, placed by its track.
            SourceRef::Boxed(scene, item) if placed.moving.is_some() => boxed_key(
                shot,
                ChainOwner::Item(scene.clone(), item.clone()),
                done,
                passes,
            ),
            SourceRef::Boxed(..) => continue,
            SourceRef::Frame(source) => GpuKey::Frame(source.clone()),
            SourceRef::Capture(spec) => GpuKey::Capture(spec.clone()),
            SourceRef::Filtered(source) => {
                let owner = ChainOwner::Source(source.clone());
                passes.filtered(shot, &owner, done);
                GpuKey::Filtered(owner)
            }
            // Its chain reads the scene at depth 1, so its nesting restarts there.
            SourceRef::Placed(source, inner) => {
                if path.contains(inner) {
                    continue;
                }
                let owner = ChainOwner::Placed(source.clone(), inner.clone());
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
            through: placed.through.clone(),
            image,
            placement: placed.placement,
            crop: placed.crop,
            opacity: placed.opacity,
            blend: placed.blend,
            boxed: placed.effects,
            moving: placed.moving,
            motion: placed.motion.clone(),
        });
    }
    path.remove(id);
    passes.pass(Pass {
        scene: id.clone(),
        depth,
        key,
        background: shot.background(id),
        camera: shot.cameras.get(id).cloned(),
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
///
/// A value addressed to a keyframe scene (#846) is kept apart from the item's own: it
/// shows only where that keyframe draws the item. Where a keyframe draws the item and
/// holds no live value of a field, the item's own live value shows if the keyframe
/// doesn't override that field, since the field follows the parent there (ADR-0049 §1).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct LiveParams {
    items: BTreeMap<ItemId, LiveItem>,
    /// Live cameras (ADR-0094), by the scene whose `scene.set_camera` set them: `None`
    /// is a live removal.
    cameras: BTreeMap<SceneId, Option<SceneCamera>>,
    /// The zoom steps `scene.zoom_camera` asked for each scene family's follow camera
    /// since the engine started (ADR-0113 §4), in (negative: out). A follow camera takes
    /// the steps asked for since it started.
    zooms: BTreeMap<SceneId, i64>,
}

/// One item's live values: its own, and those addressed to each keyframe scene.
#[derive(Clone, Debug, Default, PartialEq)]
struct LiveItem {
    own: LiveFields,
    keyframes: BTreeMap<SceneId, LiveFields>,
}

/// The live values of one item as one scene draws it: either may be absent, leaving
/// the shot's.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct LiveFields {
    placement: Option<Placement>,
    crop: Option<Crop>,
}

/// The ID a shot plans group `group` under where the keyframe scene `keyframe` draws it
/// with overrides of its children (#846, ADR-0049 §3): a view of the group with its own
/// layer, chains and boxes, since the group's own layer is shared by every scene that
/// places it. It starts with a control character and gives the group's length, so
/// [`view_of`] reads both IDs back; a scene of the collection with the same ID wins.
fn view_id(group: &SceneId, keyframe: &SceneId) -> SceneId {
    SceneId::new(format!("\u{1}{}:{group}{keyframe}", group.as_str().len()))
}

/// The ID a transition shot draws the group at track `index` under when its children
/// move inside its layer ([`transition::Inside`]), or are held there as a retargeted
/// transition found them. A control character starts it, as for [`view_id`].
fn inner_id(index: usize) -> SceneId {
    SceneId::new(format!("\u{2}{index}"))
}

/// The ID of a group at track `index` inside the group drawn as `parent`.
fn nested_inner_id(parent: &SceneId, index: usize) -> SceneId {
    SceneId::new(format!("{parent}/{index}"))
}

/// The ID a shot plans group `group` under as the keyframe scene `keyframe` draws it,
/// for tests.
#[cfg(test)]
pub(crate) fn group_view(group: &SceneId, keyframe: &SceneId) -> SceneId {
    view_id(group, keyframe)
}

/// The group and keyframe scene a [`view_id`] names.
fn view_of(id: &SceneId) -> Option<(SceneId, SceneId)> {
    let rest = id.as_str().strip_prefix('\u{1}')?;
    let (len, rest) = rest.split_once(':')?;
    let len: usize = len.parse().ok()?;
    let group = rest.get(..len)?;
    let keyframe = rest.get(len..)?;
    Some((SceneId::new(group), SceneId::new(keyframe)))
}

/// The keyframe scene a layer draws its item through, for its live values: which
/// keyframe, and which of the fields a live value can set it overrides.
#[derive(Clone, Debug, PartialEq)]
struct Through {
    keyframe: SceneId,
    geometry: bool,
    crop: bool,
}

impl Through {
    /// How `scene` draws `item`: `None` unless `scene` is a keyframe scene.
    fn of(scene: &GraphScene, item: &ItemId) -> Option<Self> {
        scene.variant_of()?;
        let overrides = scene.overrides().get(item);
        Some(Self {
            keyframe: scene.id().clone(),
            geometry: overrides.is_some_and(|o| o.geometry.is_some()),
            crop: overrides.is_some_and(|o| o.crop.is_some()),
        })
    }
}

impl LiveParams {
    /// The overrides `values` set, later ones winning. Each is an `item.set_transform`,
    /// `item.set_geometry` or `item.set_crop`, with the keyframe scene it addresses, if
    /// any; any other edit sets nothing.
    pub(crate) fn from_values<'a>(
        values: impl IntoIterator<Item = (Option<&'a SceneId>, &'a Edit)>,
    ) -> Self {
        let mut items: BTreeMap<ItemId, LiveItem> = BTreeMap::new();
        let mut cameras = BTreeMap::new();
        for (keyframe, value) in values {
            let (item_id, placement, cut) = match value {
                Edit::SceneSetCamera { scene_id, camera } => {
                    cameras.insert(scene_id.clone(), *camera);
                    continue;
                }
                Edit::ItemSetTransform {
                    item_id, transform, ..
                } => (
                    item_id,
                    Some(placement(&GraphPlacement::Legacy(*transform))),
                    None,
                ),
                Edit::ItemSetGeometry {
                    item_id, geometry, ..
                } => (
                    item_id,
                    Some(placement(&GraphPlacement::Geometry(*geometry))),
                    None,
                ),
                Edit::ItemSetCrop {
                    item_id, crop: c, ..
                } => (item_id, None, Some(crop(c))),
                _ => continue,
            };
            let item = items.entry(item_id.clone()).or_default();
            let fields = match keyframe {
                Some(keyframe) => item.keyframes.entry(keyframe.clone()).or_default(),
                None => &mut item.own,
            };
            if placement.is_some() {
                fields.placement = placement;
            }
            if cut.is_some() {
                fields.crop = cut;
            }
        }
        Self {
            items,
            cameras,
            zooms: BTreeMap::new(),
        }
    }

    /// With `zooms` as the zoom steps asked for each scene family's follow camera.
    pub(crate) fn with_zooms(mut self, zooms: BTreeMap<SceneId, i64>) -> Self {
        self.zooms = zooms;
        self
    }

    /// The zoom steps asked for `family`'s follow camera.
    pub(crate) fn zooms(&self, family: &SceneId) -> i64 {
        self.zooms.get(family).copied().unwrap_or(0)
    }

    /// The camera `pass` draws through, with live cameras applied as
    /// `State::effective_camera` applies committed ones (ADR-0094 §6): the parent's live
    /// value, a removal included, decides whether a keyframe scene is 3D, and the
    /// keyframe's own live value replaces its override.
    fn camera(&self, pass: &Pass) -> Option<SceneCamera> {
        self.camera_of(&pass.scene, pass.camera.as_ref())
    }

    /// The camera scene `scene`, planned with `camera`, draws through with its live
    /// camera applied, before its camera motion.
    fn camera_of(&self, scene: &SceneId, camera: Option<&PassCamera>) -> Option<SceneCamera> {
        let live = |scene: &SceneId| self.cameras.get(scene).copied();
        match camera {
            Some(camera) => camera.resolve(scene, live),
            None => live(scene).flatten(),
        }
    }

    /// The own live placement and crop of `item_id`, those not addressed to a keyframe
    /// scene, for tests.
    #[cfg(test)]
    pub(crate) fn item(&self, item_id: &ItemId) -> Option<(Option<Placement>, Option<Crop>)> {
        self.items
            .get(item_id)
            .map(|live| (live.own.placement, live.own.crop))
    }

    /// `placement` and `crop`, item `item`'s as a scene draws it, through the keyframe
    /// scene `through` if it is one, with its live values applied.
    fn apply_to(
        &self,
        item: &ItemId,
        through: Option<&Through>,
        placement: Placement,
        crop: Crop,
    ) -> (Placement, Crop) {
        let Some(live) = self.items.get(item) else {
            return (placement, crop);
        };
        let Some(through) = through else {
            return (
                live.own.placement.unwrap_or(placement),
                live.own.crop.unwrap_or(crop),
            );
        };
        let keyed = live.keyframes.get(&through.keyframe);
        // A field the keyframe doesn't override follows the parent, live value and all.
        (
            keyed
                .and_then(|fields| fields.placement)
                .or(live.own.placement.filter(|_| !through.geometry))
                .unwrap_or(placement),
            keyed
                .and_then(|fields| fields.crop)
                .or(live.own.crop.filter(|_| !through.crop))
                .unwrap_or(crop),
        )
    }

    /// `layer`'s placement and crop, with this item's live values applied.
    fn apply(&self, layer: &Layer) -> (Placement, Crop) {
        self.apply_to(
            &layer.item,
            layer.through.as_ref(),
            layer.placement,
            layer.crop,
        )
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

/// One request for the snapshot thread, and where to send the answer.
struct Job {
    work: Work,
    reply: ReplyTx,
}

/// What a [`Job`] draws.
enum Work {
    /// `video.snapshot`: the published render snapshot and the `rev` it answers with.
    Picture {
        snapshot: Arc<RenderSnapshot>,
        live: Arc<LiveParams>,
        rev: Rev,
        /// The moment to draw, on the media clock: where a running transition is.
        at: MediaTime,
    },
    /// A frame sheet's pictures (#1061).
    Sheet(Box<Sheet>),
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
        self.enqueue(
            Work::Picture {
                snapshot,
                live,
                rev,
                at,
            },
            reply,
        );
        answer
    }

    /// Queues a frame sheet's samples, answering through `reply` once they are drawn.
    pub(crate) fn request_sheet(&self, sheet: Sheet, reply: ReplyTx) {
        self.enqueue(Work::Sheet(Box::new(sheet)), reply);
    }

    /// Queues `work`, starting the thread if it isn't running. A failure answers
    /// `internal` through `reply`.
    fn enqueue(&self, work: Work, reply: ReplyTx) {
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
                    return;
                }
            }
        }
        if let Some((tx, _)) = queue.as_ref()
            && let Err(mpsc::SendError(job)) = tx.send(Job { work, reply })
        {
            // The thread is gone; dropping the job answers `internal`.
            drop(job);
            *queue = None;
        }
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
    for Job { work, reply } in jobs {
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let (compositor, cache, chains) = open(&mut gpu, &mut backoff)?;
            match &work {
                Work::Picture {
                    snapshot,
                    live,
                    rev,
                    at,
                } => draw(
                    snapshot.shot(),
                    live,
                    *rev,
                    *at,
                    compositor,
                    (cache, chains),
                )
                .map(|result| CallResult::Query { result }),
                Work::Sheet(sheet) => draw_sheet(sheet, compositor, (cache, chains)),
            }
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

type Answer = Result<CallResult, ProtocolError>;

#[derive(Default)]
struct Slot {
    answer: Option<Answer>,
    waker: Option<Waker>,
}

/// The sending half of a snapshot answer. Dropped without sending, it answers
/// `internal`, so a caller never waits on a job the thread lost.
pub(crate) struct ReplyTx {
    slot: Option<Arc<Mutex<Slot>>>,
}

impl ReplyTx {
    pub(crate) fn send(mut self, answer: Answer) {
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

/// A new answer slot: the sending half for whoever draws, and the future the call waits
/// on.
pub(crate) fn reply_pair() -> (ReplyTx, SnapshotReply) {
    let slot = Arc::new(Mutex::new(Slot::default()));
    (
        ReplyTx {
            slot: Some(Arc::clone(&slot)),
        },
        SnapshotReply { slot },
    )
}

/// Renders `shot` with `live` applied at the canvas size and encodes the capped PNG.
fn draw(
    shot: &Shot,
    live: &LiveParams,
    rev: Rev,
    at: MediaTime,
    compositor: &mut Compositor,
    caches: (&mut ResourceCache<GpuKey>, &mut Chains),
) -> Result<QueryResult, ProtocolError> {
    let (full_w, full_h, rgba) = render((shot, None), live, at, compositor, caches)?;
    let (width, height) = fit(full_w, full_h);
    let rgba = scale_down(full_w, full_h, &rgba, width, height);
    let png =
        encode_png(width, height, &rgba).map_err(|reason| ProtocolError::Internal { reason })?;
    Ok(QueryResult::VideoSnapshot {
        rev,
        width,
        height,
        png: base64(&png),
    })
}

/// Draws each of a frame sheet's samples at its own time, scales it to a tile and
/// encodes the tiled PNG (#1061). The layout follows the first picture's size.
fn draw_sheet(
    sheet: &Sheet,
    compositor: &mut Compositor,
    (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
) -> Answer {
    let mut layout = None;
    let mut tiles = Vec::with_capacity(sheet.samples.len());
    for sample in &sheet.samples {
        let (width, height, rgba) = render(
            (sample.snapshot.shot(), Some(&sample.pinned)),
            &sample.live,
            sample.at,
            compositor,
            (&mut *cache, &mut *chains),
        )?;
        let count = u32::try_from(sheet.samples.len()).unwrap_or(u32::MAX);
        let layout = *layout.get_or_insert_with(|| frames::Layout::new(count, (width, height)));
        tiles.push(frames::downscale((width, height), &rgba, layout.tile));
    }
    let layout = layout.ok_or_else(|| ProtocolError::Internal {
        reason: "a frame sheet with no pictures".into(),
    })?;
    let rgba = frames::assemble(&layout, &tiles);
    let png = encode_png(layout.size.0, layout.size.1, &rgba)
        .map_err(|reason| ProtocolError::Internal { reason })?;
    Ok((sheet.answer)(frames::sheet(
        &layout,
        &sheet.samples,
        sheet.start,
        base64(&png),
    )))
}

/// Renders `shot` with `live` applied at the canvas size and reads it back: its width,
/// height and tightly packed RGBA8. The images and filters `cache` no longer needs are
/// dropped here, on the snapshot thread.
fn render(
    (shot, pinned): (&Shot, Option<&Pinned>),
    live: &LiveParams,
    at: MediaTime,
    compositor: &mut Compositor,
    (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
) -> Result<(u32, u32, Vec<u8>), ProtocolError> {
    cache.begin_frame();
    let output = compose(
        compositor,
        (&mut *cache, &mut *chains),
        (shot, at),
        View {
            pinned,
            ..View::default()
        },
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
    Ok((full.width(), full.height(), full.rgba().to_vec()))
}

/// Adds one of the output stage's chains to `cost`, run over an input of `width` by
/// `height`: one layer and pass per filter, at the sizes [`Chains`] makes them.
fn add_stage(cost: &mut ChainCost, chain: &Chain, (width, height): (u32, u32)) {
    let sizes = chain.layer_sizes(width, height);
    let count = u32::try_from(sizes.len()).unwrap_or(u32::MAX);
    cost.stage_chains = cost.stage_chains.saturating_add(1);
    cost.stage_layers = cost.stage_layers.saturating_add(count);
    cost.stage_passes = cost.stage_passes.saturating_add(count);
    let bytes = sizes
        .into_iter()
        .map(layer_bytes)
        .fold(0, u64::saturating_add);
    cost.layer_bytes = cost.layer_bytes.saturating_add(bytes);
}

/// The bytes of one pixel of an offscreen layer (`Rgba16Float`).
const LAYER_PIXEL_BYTES: u64 = 8;

/// The texture bytes of a layer of `width` by `height`.
fn layer_bytes((width, height): (u32, u32)) -> u64 {
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(LAYER_PIXEL_BYTES)
}

impl Shot {
    /// What the scene chains, item effects and output stage add to each frame
    /// (ADR-0065 §7, ADR-0080), for `stats.tick`. The scene chains and item effects are
    /// read from the chain phase's steps ([`schedule_chains`]), so it counts what each
    /// frame runs once, before any mix draws. A scene's chain is its canvas-sized
    /// composite and one layer and pass per filter; an item's effects are its box's
    /// layer and pass and one per effect, at the sizes [`Chains`] makes them, plus one
    /// layer and pass for the copy into the grown layer where the chain is padded
    /// (ADR-0125, [`Chain::pad_layer`]). A filter's own scratch layers (a blur's) are
    /// not counted. A source's
    /// chain (a `scene` source's included), a scene a `scene_mask` or a `scene` source's
    /// chain reads, and the reference layer a nested scene
    /// without a chain is drawn into for an item's effects (in place of the layer it
    /// would be drawn into anyway) are not counted.
    ///
    /// The output stage runs only for an output's frame, so it is counted for
    /// `running`, the mix and owner each running output's frames are drawn with (its
    /// session's, as the render thread draws `View::output`): what [`Shot::stage_of`]
    /// gives that frame, each mix's canvas-sized layer and master chain once however
    /// many outputs get it ([`Chains::stage`]), and the owner's own chain over what the
    /// master chain makes. The pass fitting the result into the output's frame, which
    /// every output frame draws, is not counted.
    pub(crate) fn chain_cost<'a>(
        &self,
        running: impl IntoIterator<Item = (Option<&'a VideoMixId>, Option<&'a OutputId>)>,
    ) -> ChainCost {
        let mut cost = ChainCost::default();
        let canvas = (self.canvas.width, self.canvas.height);
        for step in &self.chain_steps {
            let ChainStep::Run(owner) = step else {
                continue;
            };
            let Some(chain) = self.chains.get(owner) else {
                continue;
            };
            let (input, layers, passes) = match owner {
                ChainOwner::Scene(_) => {
                    cost.scene_chains = cost.scene_chains.saturating_add(1);
                    (canvas, &mut cost.scene_layers, &mut cost.scene_passes)
                }
                ChainOwner::Item(..) => {
                    let Some(boxed) = self.boxes.get(owner) else {
                        continue;
                    };
                    cost.item_effects = cost.item_effects.saturating_add(1);
                    let [width, height] = boxed.boxed.layer;
                    (
                        (width, height),
                        &mut cost.item_layers,
                        &mut cost.item_passes,
                    )
                }
                ChainOwner::Source(_)
                | ChainOwner::Placed(..)
                | ChainOwner::Master(_)
                | ChainOwner::Output(_) => continue,
            };
            let sizes = chain.layer_sizes(input.0, input.1);
            // An item's chain padded where an effect first draws past the box copies
            // that effect's input into a grown layer first (ADR-0125, #1285).
            let pad = chain.pad_layer(input.0, input.1);
            // The composite or the box, then one per filter, and the pad copy.
            let count = u32::try_from(sizes.len())
                .unwrap_or(u32::MAX)
                .saturating_add(1)
                .saturating_add(u32::from(pad.is_some()));
            *layers = layers.saturating_add(count);
            *passes = passes.saturating_add(count);
            let bytes = sizes
                .into_iter()
                .chain(pad)
                .map(layer_bytes)
                .fold(layer_bytes(input), u64::saturating_add);
            cost.layer_bytes = cost.layer_bytes.saturating_add(bytes);
        }
        let mut staged = BTreeSet::new();
        for (mix, owner) in running {
            let Some(stage) = self.stage_of(mix, owner) else {
                continue;
            };
            // What the output's own chain reads: the master chain's output, or the mix.
            let mut size = canvas;
            if staged.insert(mix) {
                cost.stage_passes = cost.stage_passes.saturating_add(1);
                cost.stage_layers = cost.stage_layers.saturating_add(1);
                cost.layer_bytes = cost.layer_bytes.saturating_add(layer_bytes(canvas));
                if let Some((_, master)) = stage.master {
                    add_stage(&mut cost, master, canvas);
                }
            }
            if let Some((_, master)) = stage.master {
                size = master.output_size(canvas.0, canvas.1);
            }
            if let Some((_, own)) = stage.output {
                add_stage(&mut cost, own, size);
            }
        }
        cost
    }

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

impl CaptureFrame<'_> {
    /// Its width and height, in pixels.
    pub(crate) fn size(&self) -> (f32, f32) {
        let size = match self.image {
            CaptureImage::Rgba { size, .. } => size,
            CaptureImage::Cpu(frame) => frame.info().size,
            CaptureImage::Gpu(frame) => frame.info().size,
        };
        (size.width() as f32, size.height() as f32)
    }
}

/// Where a [`CaptureFrame`]'s pixels are.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CaptureImage<'a> {
    /// Tightly packed straight sRGB RGBA8 at `size`, uploaded on a new version.
    Rgba { size: FrameSize, rgba: &'a [u8] },
    /// A capture's CPU frame as it arrived (packed RGBA8 or BGRA8, or NV12), read as
    /// opaque and converted on the GPU as it is uploaded on a new version
    /// ([`ResourceCache::upload_frame`], ADR-0117 §2), so the drawing thread never
    /// converts it pixel by pixel.
    Cpu(&'a jstream_media::VideoFrame),
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

    /// Where the pointer is over `spec`'s capture, on its newest frame
    /// (`CaptureStream::pointer`), for a follow camera (ADR-0113); `None` when it doesn't
    /// say. Never blocks.
    fn pointer(&self, _spec: &CaptureKey) -> Option<jstream_platform::PointerPosition> {
        None
    }
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
/// transition samples it) at `to`'s size, over opaque black, and reads it back when `to`
/// asks for that,
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
    shot: (&Shot, MediaTime),
    view: View<'_>,
    live: &LiveParams,
    captures: &dyn Captures,
    to: DrawAt,
) -> Result<Output, RenderError> {
    compose_metered(
        compositor,
        (cache, chains, &mut DrawMeter::default()),
        shot,
        view,
        live,
        captures,
        to,
    )
}

/// What `source`'s placements need of `frame`'s resolution over every mix, due this tick
/// or not, so a mix that only claims its layers doesn't let its upload shrink
/// ([`ResourceCache::upload_drawn`], #1139). `scales` holds [`Shot::scales`], measured
/// once per compose and only when a frame needs it.
fn planned_need(
    scales: &mut Option<BTreeMap<SourceId, f32>>,
    shot: &Shot,
    source: &SourceId,
    frame: &Frame,
) -> f32 {
    let scales = scales.get_or_insert_with(|| shot.scales());
    scales.get(source).map_or(0.0, |scale| {
        scale * frame.extent[0] as f32 / frame.width.max(1) as f32
    })
}

/// [`compose`], timing each unit of the work into `meter` (ADR-0103): each source's
/// upload, each capture, each video chain (an item's with its box), each nested pass and
/// the composite, and the phases they make up. The readback's wait counts toward the
/// finish phase, not the composite. The caller ends the meter's frame.
///
/// # Errors
/// As for [`compose`].
pub(crate) fn compose_metered(
    compositor: &mut Compositor,
    (cache, chains, meter): (&mut ResourceCache<GpuKey>, &mut Chains, &mut DrawMeter),
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
    compositor.set_blending(shot.blending);
    // A follow camera no frame has drawn for a while starts again at rest.
    chains.cameras().sweep(at);
    let uploads = Instant::now();
    let mut planned = None;
    for (key, source) in &shot.uploads {
        let Some(frame) = shot.frames.get(source) else {
            continue;
        };
        let started = Instant::now();
        match &frame.pixels {
            // Sampled where it is, on the context it was painted on. Another compositor
            // (`video.snapshot`'s, or a program stage opened since) can't read it.
            Pixels::Gpu { frame, context } => {
                if compositor.context() == Some(context) {
                    cache.import_premultiplied(compositor, key, frame)?;
                }
            }
            // Held at the size its items draw it, unless something reads its texels
            // as they are: a video chain, or a model it stands in for (#1139).
            Pixels::Copied(_) | Pixels::Video(_) if shot.drawn_only(key, frame) => {
                let planned = planned_need(&mut planned, shot, source, frame);
                cache.upload_drawn(
                    compositor,
                    key,
                    frame.version,
                    (frame.width, frame.height),
                    frame.rgba(),
                    planned,
                )?;
                if let Ok(extent) = FrameSize::new(frame.extent[0], frame.extent[1]) {
                    cache.set_extent(key, extent);
                }
            }
            Pixels::Slot(slot) => {
                // Read without a lock, and let go of here without freeing the frame:
                // the slot's writers free what they replace (R-RT-003).
                let latest = view
                    .pinned
                    .and_then(|pinned| pinned.0.get(source))
                    .map_or_else(|| slot.latest(), Arc::clone);
                let size = latest.frame().info().size;
                // Only a frame of the planned size is placed as planned; one of another
                // size waits for the plan its producer asked for.
                if [size.width(), size.height()] == [frame.width, frame.height] {
                    if shot.drawn_only(key, frame) {
                        let planned = planned_need(&mut planned, shot, source, frame);
                        cache.upload_drawn(
                            compositor,
                            key,
                            latest.version(),
                            (size.width(), size.height()),
                            latest.rgba(),
                            planned,
                        )?;
                    } else {
                        cache.upload(
                            compositor,
                            key,
                            latest.version(),
                            size.width(),
                            size.height(),
                            latest.rgba(),
                        )?;
                    }
                    if let Ok(extent) = FrameSize::new(frame.extent[0], frame.extent[1]) {
                        cache.set_extent(key, extent);
                    }
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
        meter.upload(source, started);
    }
    meter.phase_since(Phase::Uploads, uploads);
    let passes = shot.passes_of(view.mix);
    let tracks = shot
        .transition
        .as_ref()
        .filter(|_| shot.shows_program(view.mix));
    let mut fallback = None;
    let capturing = Instant::now();
    for spec in &shot.captures {
        let Some(capture) = captures.frame(spec) else {
            continue;
        };
        let started = Instant::now();
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
            CaptureImage::Cpu(frame) => {
                cache.upload_frame(compositor, &key, capture.version, frame)?
            }
            // Opaque, as the CPU path ignores a capture's alpha (#451).
            CaptureImage::Gpu(frame) => cache.import_opaque(compositor, &key, frame)?,
        };
        meter.capture(spec, started);
        fallback = Some(key);
    }
    meter.phase_since(Phase::Captures, capturing);
    let chaining = Instant::now();
    let ran = run_chains(
        compositor,
        (&mut *cache, &mut *chains, &mut *meter),
        shot,
        (live, captures),
        at,
    );
    meter.phase_since(Phase::Chains, chaining);
    ran?;
    let stage = view
        .stage
        .then(|| shot.stage_of(view.mix, view.owner))
        .flatten();
    // The mix goes to the output stage's layer at the canvas size, or straight to the
    // output.
    let target = if stage.is_some() { canvas } else { output };
    if let Some(stage) = &stage
        && !chains.stage(&stage.mixed)
    {
        // Another output's frame drew this mix and ran its master stage this tick.
        return finish_stage(
            compositor,
            (cache, chains, meter),
            stage,
            at,
            (output, readback),
        );
    }
    let (root, nested) = match (tracks, passes.split_last()) {
        (Some(_), _) => (None, passes),
        (None, Some((root, nested))) => (Some(root), nested),
        (None, None) => {
            if let Some(stage) = &stage {
                cache.layer(compositor, &stage.mixed, canvas.width(), canvas.height())?;
            }
            {
                let image = fallback.as_ref().and_then(|key| cache.get(key));
                // Scaled to fill the canvas, which the output follows (not the display),
                // and fitted inside the output like the canvas layer.
                let (scale, offset) = fit_inside(canvas, target);
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
                            blend: BlendMode::Normal,
                            visible: true,
                            camera: None,
                            depth_run: 0,
                        }
                    })
                    .collect();
                let shown = scene(target, Color::BLACK, &items);
                let Some(stage) = &stage else {
                    return composite(compositor, meter, &shown, readback);
                };
                let layer = cache
                    .get(&stage.mixed)
                    .ok_or_else(|| RenderError::Invalid {
                        what: "cache",
                        detail: format!("no image for {:?}", stage.mixed),
                    })?;
                let started = Instant::now();
                compositor.render_into(&shown, layer)?;
                let took = meter.phase_since(Phase::Passes, started);
                meter.composite(took);
            }
            return match &stage {
                Some(stage) => finish_stage(
                    compositor,
                    (cache, chains, meter),
                    stage,
                    at,
                    (output, readback),
                ),
                None => Err(RenderError::Invalid {
                    what: "output stage",
                    detail: "a frame without an output stage reached it".into(),
                }),
            };
        }
    };
    let direct = target == canvas;
    claim_layers(compositor, (&mut *cache, &mut *chains), shot, view, output)?;
    {
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
            // The chains drew this scene at this depth already this frame, for a
            // chain that reads it: the same pass, so its layer holds what this one
            // would draw (#1070).
            if chains.reuse(&pass.key) {
                continue;
            }
            let started = Instant::now();
            fill_items(
                &mut items,
                (cache, chains),
                (shot, pass),
                (at, canvas),
                (live, captures),
            );
            compositor.render_into(&scene(canvas, pass.background, &items), image(&pass.key)?)?;
            let took = meter.phase_since(Phase::Passes, started);
            meter.pass(&pass.scene, took);
        }
        let started = Instant::now();
        // Where the mix shows a transparent pixel, the output is black (ADR-0024 §4).
        let clear = match (root, tracks) {
            (_, Some(transition)) => {
                fill_tracks(
                    &mut items,
                    (cache, chains),
                    (shot, transition),
                    canvas,
                    at,
                    (live, captures),
                );
                transition.background(at)
            }
            (Some(root), None) => {
                fill_items(
                    &mut items,
                    (cache, chains),
                    (shot, root),
                    (at, canvas),
                    (live, captures),
                );
                root.background
            }
            (None, None) => {
                items.clear();
                Color::TRANSPARENT
            }
        }
        .over_black();
        match &stage {
            Some(stage) => {
                compositor.render_into(&scene(canvas, clear, &items), image(&stage.mixed)?)?;
                let took = meter.phase_since(Phase::Passes, started);
                meter.composite(took);
            }
            None if direct => {
                let shown = scene(output, clear, &items);
                return composite_from(compositor, meter, &shown, readback, started);
            }
            None => {
                let layer = image(&GpuKey::Program)?;
                compositor.render_into(&scene(canvas, clear, &items), layer)?;
                let placed = [fitted(layer, canvas, output)];
                let shown = scene(output, Color::BLACK, &placed);
                return composite_from(compositor, meter, &shown, readback, started);
            }
        }
    }
    match &stage {
        Some(stage) => finish_stage(
            compositor,
            (cache, chains, meter),
            stage,
            at,
            (output, readback),
        ),
        // Every path without a stage returned above.
        None => Err(RenderError::Invalid {
            what: "output stage",
            detail: "a frame without an output stage reached it".into(),
        }),
    }
}

/// Runs the output stage's chains of `stage` over its mix's layer, already drawn this
/// frame, and draws what they make into an `output`-sized frame, fitted inside it like
/// the canvas layer (ADR-0080): the master chain once per frame of the cache, however
/// many outputs draw the mix, then the output's own. A chain that failed this frame
/// draws a black frame, as a failed chain draws nothing elsewhere: the unfiltered mix
/// never leaves in its place.
///
/// # Errors
/// What the compositor reports for the frame's pass.
fn finish_stage(
    compositor: &mut Compositor,
    (cache, chains, meter): (&mut ResourceCache<GpuKey>, &mut Chains, &mut DrawMeter),
    stage: &Stage<'_>,
    at: MediaTime,
    (output, readback): (FrameSize, bool),
) -> Result<Output, RenderError> {
    let started = Instant::now();
    let mut shown = None;
    let mut failed = false;
    for (owner, chain) in [stage.master, stage.output].into_iter().flatten() {
        let key = GpuKey::Filtered(owner.clone());
        if chains.stage(&key) {
            let running = Instant::now();
            chains.run_one(compositor, cache, (owner, chain), at);
            meter.chain(owner, running);
        }
        failed |= chains.failed(&key);
        shown = Some(key);
    }
    let shown = shown.as_ref().unwrap_or(&stage.mixed);
    // Fitted at its own size: an aspect ratio filter changes it.
    let placed: Vec<Item<'_>> = cache
        .get(shown)
        .filter(|_| !failed)
        .and_then(|image| {
            let size = FrameSize::new(image.width(), image.height()).ok()?;
            Some(fitted(image, size, output))
        })
        .into_iter()
        .collect();
    let drawn = compositor.render(&scene(output, Color::BLACK, &placed), readback);
    meter.phase_since(Phase::Finish, started);
    drawn
}

/// `compositor.render` of `shown`, the program scene's composite into the frame, timed
/// into `meter` from `started`: the readback's wait counts toward the finish phase, the
/// rest toward the composite.
fn composite_from(
    compositor: &mut Compositor,
    meter: &mut DrawMeter,
    shown: &Scene<'_>,
    readback: bool,
    started: Instant,
) -> Result<Output, RenderError> {
    let drawn = compositor.render(shown, readback);
    let took = started.elapsed();
    let waited = compositor.readback_wait().min(took);
    meter.phase(Phase::Finish, waited);
    meter.phase(Phase::Passes, took.saturating_sub(waited));
    meter.composite(took.saturating_sub(waited));
    drawn
}

/// [`composite_from`] starting now.
fn composite(
    compositor: &mut Compositor,
    meter: &mut DrawMeter,
    shown: &Scene<'_>,
    readback: bool,
) -> Result<Output, RenderError> {
    composite_from(compositor, meter, shown, readback, Instant::now())
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
    (cache, chains, meter): (&mut ResourceCache<GpuKey>, &mut Chains, &mut DrawMeter),
    shot: &Shot,
    (live, captures): (&LiveParams, &dyn Captures),
    at: MediaTime,
) -> Result<(), RenderError> {
    // A Move blending an item's effects gives its chain this frame's settings (ADR-0127).
    let blended = shot
        .transition
        .as_ref()
        .filter(|transition| !transition.blends.is_empty())
        .map(|transition| transition.blended(at))
        .unwrap_or_default();
    if !chains.begin(compositor, cache, &shot.chains, &shot.stages, &blended) {
        return Ok(());
    }
    let canvas = shot.canvas()?;
    for step in &shot.chain_steps {
        let started = Instant::now();
        match step {
            ChainStep::Run(source) => {
                if let Some(chain) = shot.chains.get(source) {
                    chains.run_one(compositor, cache, (source, chain), at);
                    meter.chain(source, started);
                }
            }
            ChainStep::Box(owner) => {
                if let Some(boxed) = shot.boxes.get(owner) {
                    // A layer too large for the adapter blanks its item, not the frame.
                    match draw_box(
                        compositor,
                        (&mut *cache, &mut *chains),
                        (shot, at),
                        owner,
                        boxed,
                        (live, captures),
                    ) {
                        Ok(()) => chains.drew_box(owner),
                        Err(error) => chains.fail_box(owner, &error),
                    }
                    // The box counts toward the item's effects, which run next.
                    meter.chain(owner, started);
                }
            }
            ChainStep::Pass(pass) => {
                cache.layer(compositor, &pass.key, canvas.width(), canvas.height())?;
                let mut items = Vec::new();
                fill_items(
                    &mut items,
                    (&*cache, &*chains),
                    (shot, pass),
                    (at, canvas),
                    (live, captures),
                );
                let Some(layer) = cache.get(&pass.key) else {
                    continue;
                };
                compositor.render_into(&scene(canvas, pass.background, &items), layer)?;
                chains.drew_layer(&pass.key);
                meter.pass(&pass.scene, started.elapsed());
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
    (shot, at): (&Shot, MediaTime),
    owner: &ChainOwner,
    boxed: &ItemBox,
    (live, captures): (&LiveParams, &dyn Captures),
) -> Result<(), RenderError> {
    let ChainOwner::Item(scene_id, item_id) = owner else {
        return Ok(());
    };
    let key = GpuKey::ItemBox(scene_id.clone(), item_id.clone());
    let [width, height] = boxed.boxed.layer;
    cache.layer(compositor, &key, width, height)?;
    if boxed.boxed.model {
        return draw_model(
            compositor,
            (cache, chains),
            (shot, at),
            (owner, &key),
            boxed,
            (live, captures),
        );
    }
    let input = box_input(shot, &boxed.source).filter(|input| !chains.failed(input));
    let (placement, crop) = live.apply_to(
        &boxed.item,
        boxed.through.as_ref(),
        boxed.placement,
        boxed.crop,
    );
    let by = shot.motion_at(&boxed.item, &boxed.motion, at);
    let (placement, crop) = boxed.boxed.moved_inside(&by, placement, crop);
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
    // Flat in its box: the box itself is placed in 3D where its item is.
    let items = [Item {
        image,
        placement: Placement::Geometry(inside),
        crop,
        opacity: OPAQUE,
        blend: BlendMode::Normal,
        visible: true,
        camera: None,
        depth_run: 0,
    }];
    let size = FrameSize::new(width, height).map_err(|_| RenderError::Invalid {
        what: "item box",
        detail: format!("an effects layer of {width}x{height}"),
    })?;
    compositor.render_into(&scene(size, Color::TRANSPARENT, &items), layer)
}

/// Draws the 3D model item `boxed` shows into its canvas-sized layer `key` (ADR-0107
/// decision 6): posed by its geometry with its live values and motion applied, seen
/// through its scene's camera as it moves this frame, and lit as its source says. The
/// model is uploaded once per version and kept ([`Chains::model`]). One that isn't
/// there this frame, or can't be uploaded, fails its item, which then draws nothing.
///
/// # Errors
/// What the compositor reports for drawing into the layer.
fn draw_model(
    compositor: &mut Compositor,
    (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
    (shot, at): (&Shot, MediaTime),
    (owner, key): (&ChainOwner, &GpuKey),
    boxed: &ItemBox,
    (live, captures): (&LiveParams, &dyn Captures),
) -> Result<(), RenderError> {
    let ChainOwner::Item(scene_id, _) = owner else {
        return Ok(());
    };
    let shown = match &boxed.source {
        SourceRef::Frame(id) => shot
            .frames
            .get(id)
            .and_then(|frame| frame.model.as_ref())
            .map(|shown| (id, shown)),
        _ => None,
    };
    let (placement, crop) = live.apply_to(
        &boxed.item,
        boxed.through.as_ref(),
        boxed.placement,
        boxed.crop,
    );
    let by = shot.motion_at(&boxed.item, &boxed.motion, at);
    let (placement, _) = boxed.boxed.moved_inside(&by, placement, crop);
    let (Some((source, shown)), Placement::Geometry(geometry)) = (shown, placement) else {
        chains.fail(owner);
        return Ok(());
    };
    let camera = moved_camera(
        live.camera_of(scene_id, boxed.camera.as_ref()),
        boxed.camera.as_ref(),
        (shot, at, shot.shown_at(&boxed.item)),
        (live, captures),
        chains,
    );
    if let Err(error) = chains.upload_model(compositor, source, &shown.model) {
        chains.fail_box(owner, &error);
        return Ok(());
    }
    let (Some(model), Some(layer)) = (chains.model(source), cache.get(key)) else {
        chains.fail(owner);
        return Ok(());
    };
    compositor.draw_model(
        &jstream_render::ModelDraw {
            model,
            geometry,
            camera,
            lighting: shown.lighting,
        },
        layer,
    )
}

/// Marks the layers [`compose`] draws `view` at `output` through as used in the cache's
/// current frame, allocating any that are missing, without drawing: each nested
/// scene's, the program layer when `output` isn't the canvas size, or for an output's
/// frame with an output stage the mix's layer ([`GpuKey::Mixed`]), and the layers of the
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
    view: View<'_>,
    output: FrameSize,
) -> Result<(), RenderError> {
    chains.claim(compositor, cache);
    let canvas = shot.canvas()?;
    let mix = view.mix;
    // The output stage's layer, which a frame for an output with a stage draws into
    // instead of the output or the program layer.
    let staged = view.stage && shot.stage_of(mix, view.owner).is_some();
    if staged {
        cache.layer(
            compositor,
            &GpuKey::Mixed(mix.cloned()),
            canvas.width(),
            canvas.height(),
        )?;
    }
    for step in &shot.chain_steps {
        match step {
            ChainStep::Pass(pass) => {
                cache.layer(compositor, &pass.key, canvas.width(), canvas.height())?;
            }
            ChainStep::Box(owner) => {
                if let (ChainOwner::Item(scene, item), Some(boxed)) = (owner, shot.boxes.get(owner))
                {
                    let [width, height] = boxed.boxed.layer;
                    // One that can't be made fails its item when the chains next draw.
                    let _ = cache.layer(
                        compositor,
                        &GpuKey::ItemBox(scene.clone(), item.clone()),
                        width,
                        height,
                    );
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
    if output != canvas && !staged {
        cache.layer(
            compositor,
            &GpuKey::Program,
            canvas.width(),
            canvas.height(),
        )?;
    }
    Ok(())
}

/// Replaces `items` with the layers of `pass`, with the live values in `live` applied,
/// and a moving layer's track sampled at `at` on a canvas of `canvas`. A layer whose
/// image is not cached (a failed or skipped upload), or whose video chain failed this
/// frame, draws nothing.
fn fill_items<'a>(
    items: &mut Vec<Item<'a>>,
    (cache, chains): (&'a ResourceCache<GpuKey>, &Chains),
    (shot, pass): (&Shot, &Pass),
    (at, canvas): (MediaTime, FrameSize),
    (live, captures): (&LiveParams, &dyn Captures),
) {
    let size = [canvas.width() as f32, canvas.height() as f32];
    let camera = moved_camera(
        live.camera(pass),
        pass.camera.as_ref(),
        (shot, at, shot.pass_shown(pass)),
        (live, captures),
        chains,
    );
    items.clear();
    items.extend(pass.layers.iter().filter_map(|layer| {
        if chains.failed(&layer.image) {
            return None;
        }
        if let Some((start, track)) = &layer.moving {
            let elapsed = transition::elapsed_ms(*start, at);
            let by = layer
                .motion
                .offsets(track, elapsed, |item, m| shot.motion_at(item, m, at));
            let sampled = moved_track(
                shot.is_canvas_box(&layer.image),
                &by,
                track.sample(elapsed, size)?,
            );
            return Some(Item {
                image: cache.get(&layer.image)?,
                placement: track_placement(shot, &layer.image, sampled.geometry),
                crop: sampled.crop,
                opacity: sampled.opacity,
                blend: sampled.blend,
                visible: true,
                camera,
                depth_run: 0,
            });
        }
        let (placement, crop) = live.apply(layer);
        // Its motion moves it on top of its live values (ADR-0099).
        let (item, modifiers) = &layer.motion.to;
        let by = shot.motion_at(item, modifiers, at);
        let (placement, crop, opacity) = motion::apply(&by, placement, crop, layer.opacity);
        let (placement, crop) = match &layer.boxed {
            // A model's layer is in the canvas's pixels; it sorts by the model's pose.
            Some(boxed) if boxed.model => match placement {
                Placement::Geometry(geometry) => (Placement::Layer(geometry), Crop::NONE),
                _ => return None,
            },
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
            opacity,
            blend: layer.blend,
            visible: true,
            camera,
            depth_run: 0,
        })
    }));
}

/// `base`, a pass's camera with its live camera applied, moved by its family's camera
/// motion and follow camera (ADR-0113) at `at`, for a pass first shown at `shown`.
fn moved_camera(
    base: Option<SceneCamera>,
    planned: Option<&PassCamera>,
    (shot, at, shown): (&Shot, MediaTime, Option<MediaTime>),
    (live, captures): (&LiveParams, &dyn Captures),
    chains: &Chains,
) -> Option<SceneCamera> {
    let base = base?;
    Some(match planned.and_then(|camera| camera.moves.as_deref()) {
        Some(moves) => moves.apply(
            base,
            (at, shown),
            shot.canvas,
            captures,
            &mut chains.cameras(),
            live.zooms(&moves.family),
        ),
        None => base,
    })
}

/// Where a transition track of `image` is placed at `geometry`: a model's layer
/// (ADR-0107), already in the canvas's pixels, flat, and anything else in its box.
fn track_placement(shot: &Shot, image: &GpuKey, geometry: ItemGeometry) -> Placement {
    if shot.is_model_box(image) {
        Placement::Layer(geometry)
    } else {
        Placement::Geometry(geometry)
    }
}

/// A track's sampled endpoint moved by `by`, its item's motion. A canvas-space layer
/// (ADR-0106) stays over the canvas: its item moves inside it ([`draw_box`]), so only
/// the layer's opacity takes the motion here.
fn moved_track(canvas_box: bool, by: &jstream_anim::Offsets, sampled: Endpoint) -> Endpoint {
    if canvas_box {
        Endpoint {
            opacity: motion::opacity_of(by, sampled.opacity),
            ..sampled
        }
    } else {
        motion::apply_endpoint(by, sampled)
    }
}

/// Replaces `items` with `transition`'s tracks sampled at `at`, in drawing order. A
/// track that draws nothing then, or whose image is not cached, is left out.
fn fill_tracks<'a>(
    items: &mut Vec<Item<'a>>,
    (cache, chains): (&'a ResourceCache<GpuKey>, &Chains),
    (shot, transition): (&Shot, &Transitioning),
    canvas: FrameSize,
    at: MediaTime,
    (live, captures): (&LiveParams, &dyn Captures),
) {
    let elapsed = transition::elapsed_ms(transition.start, at);
    let size = [canvas.width() as f32, canvas.height() as f32];
    // Each end's camera as it moves this frame, a follow camera's included.
    let mut cameras = transition.cameras;
    let moved = |moves: &Option<Arc<CameraMoves>>, camera: Option<SceneCamera>, shown| {
        let (Some(moves), Some(camera)) = (moves, camera) else {
            return camera;
        };
        Some(moves.apply(
            camera,
            (at, shown),
            shot.canvas,
            captures,
            &mut chains.cameras(),
            live.zooms(&moves.family),
        ))
    };
    cameras.ends = (
        moved(&transition.moves.0, cameras.ends.0, None),
        moved(&transition.moves.1, cameras.ends.1, Some(transition.start)),
    );
    items.clear();
    items.extend(
        transition
            .tracks
            .iter()
            .filter_map(|(key, track, seen, moved)| {
                if chains.failed(key) {
                    return None;
                }
                let by = moved.offsets(track, elapsed, |item, m| shot.motion_at(item, m, at));
                let sampled =
                    moved_track(shot.is_canvas_box(key), &by, track.sample(elapsed, size)?);
                Some(Item {
                    image: cache.get(key)?,
                    placement: track_placement(shot, key, sampled.geometry),
                    crop: sampled.crop,
                    opacity: sampled.opacity,
                    blend: sampled.blend,
                    visible: true,
                    camera: cameras.camera(track, *seen, elapsed),
                    // A stinger's items sort on their own, over the scenes under them.
                    depth_run: transition.cameras.depth_run(track, *seen),
                })
            }),
    );
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
        blend: BlendMode::Normal,
        visible: true,
        camera: None,
        depth_run: 0,
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

/// A render error as the caller sees it. No adapter is the machine's setup, not an engine
/// fault, so it is `action_failed`, as `output.start` reports it, with the driver to
/// install named on Linux (#1073); every other failure is `internal`.
fn from_render(err: RenderError) -> ProtocolError {
    if err.is_no_adapter() {
        ProtocolError::ActionFailed {
            reason: err.to_string(),
        }
    } else {
        ProtocolError::Internal {
            reason: err.to_string(),
        }
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

/// `rgba` as a PNG, for `video.snapshot` and a preview stream's `png` codec.
pub(crate) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
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

/// Standard base64 with padding and no line breaks: a `video.snapshot`'s PNG, and an
/// image `pack.read` returns.
pub(crate) fn base64(data: &[u8]) -> String {
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

/// Standard base64 with padding decoded, strictly: `=` only at the end of the last
/// four characters. For a `pack.write`'s images (ADR-0097), which a client sends, and
/// tests that check `video.snapshot`'s `png` field.
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
    let last = bytes.len() / 4;
    for (index, chunk) in bytes.chunks_exact(4).enumerate() {
        let pad = chunk.iter().rev().take_while(|b| **b == b'=').count();
        if pad > 2 {
            return Err("too much base64 padding".into());
        }
        if (pad > 0 && index + 1 != last) || chunk[..4 - pad].contains(&b'=') {
            return Err("base64 padding before the end".into());
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
    matches!(
        error,
        ProtocolError::ActionFailed { reason } | ProtocolError::Internal { reason }
            if reason.starts_with("no wgpu adapter")
    )
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
        BlendMode, Canvas, Edit, ItemId, ProtocolError, QueryResult, Rev, SceneId, SourceId,
    };
    use jstream_render::{
        Compositor, Crop, LegacyTransform, OpenOptions, Placement, RenderError, ResourceCache,
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

    /// An item's effects layer is its box, which the effects pad by the outset on every
    /// side (ADR-0065 §5): the box is drawn into it, flat, and the result is placed over
    /// the box grown
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
        // The box alone: the effects pad it by the outset as they run.
        assert_eq!(boxed.layer, [100, 50]);

        let inside = boxed.inside(&placed).expect("inside");
        assert_eq!((inside.center.x.get(), inside.center.y.get()), (50.0, 25.0));
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

    /// An item whose effects run in canvas space (ADR-0106) is drawn into a canvas-sized
    /// layer where it is placed, turned and flipped as it is, and the layer covers the
    /// canvas flat. A canvas too large for one texture is scaled down uniformly.
    #[test]
    fn a_canvas_space_effects_layer_is_the_canvas() {
        use jstream_protocol::{Canvas, Finite32, FitMode, ItemGeometry, Size, Vec2};
        let mut placed = ItemGeometry::try_new(
            Vec2::new(500.0, 300.0).expect("finite"),
            Size::try_new(100.0, 50.0).expect("positive"),
        )
        .expect("valid");
        placed.rotation_deg = Finite32::new(30.0).expect("finite");
        placed.flip_v = true;
        let canvas = Canvas {
            width: 1920,
            height: 1080,
        };
        let boxed = super::Boxed::over_canvas(canvas, None).expect("finite");
        assert_eq!(boxed.layer, [1920, 1080]);

        let inside = boxed.inside(&placed).expect("inside");
        assert_eq!(inside, placed, "placed in the layer as on the canvas");

        let around = boxed.around(&placed).expect("around");
        assert_eq!(
            (around.center.x.get(), around.center.y.get()),
            (960.0, 540.0)
        );
        assert_eq!(
            (around.size.width.get(), around.size.height.get()),
            (1920.0, 1080.0)
        );
        assert!(!around.flip_v && around.rotation_deg.get() == 0.0);
        assert_eq!(around.fit_mode, FitMode::Stretch);

        let huge = Canvas {
            width: 16_384,
            height: 4_096,
        };
        let scaled = super::Boxed::over_canvas(huge, None).expect("finite");
        assert_eq!((scaled.layer, scaled.scale), ([8192, 2048], 0.5));
        let inside = scaled.inside(&placed).expect("inside");
        assert_eq!(
            (inside.center.x.get(), inside.center.y.get()),
            (250.0, 150.0)
        );
        assert_eq!(inside.size.width.get(), 50.0);
        assert!(inside.flip_v, "the item's own flip");
    }

    /// Motion on an item whose effects run over the canvas (ADR-0106, ADR-0099) moves
    /// the item inside its layer, which stays over the canvas, on a steady frame and
    /// during a switch alike; a box's layer moves where it is placed and only cuts the
    /// crop inside (#1125 review B8).
    #[test]
    fn motion_moves_a_canvas_space_item_inside_its_layer() {
        use jstream_protocol::{Canvas, ItemGeometry, Size, Vec2};
        let placed = ItemGeometry::try_new(
            Vec2::new(500.0, 300.0).expect("finite"),
            Size::try_new(100.0, 50.0).expect("positive"),
        )
        .expect("valid");
        let by = jstream_anim::Offsets {
            x: 40.0,
            opacity: 0.5,
            crop: [2.0, 0.0, 0.0, 0.0],
            ..jstream_anim::Offsets::IDENTITY
        };
        let canvas = super::Boxed::over_canvas(
            Canvas {
                width: 1920,
                height: 1080,
            },
            None,
        )
        .expect("finite");
        let (moved, crop) = canvas.moved_inside(&by, Placement::Geometry(placed), Crop::NONE);
        let Placement::Geometry(moved) = moved else {
            panic!("a geometry");
        };
        assert_eq!(moved.center.x.get(), 540.0, "moved inside the layer");
        assert_eq!(crop.left, 2);
        let inside = canvas.inside(&moved).expect("inside");
        assert_eq!(inside.center.x.get(), 540.0);

        let boxed = super::Boxed::new(&placed, 4, None).expect("finite");
        let (kept, crop) = boxed.moved_inside(&by, Placement::Geometry(placed), Crop::NONE);
        assert_eq!(
            kept,
            Placement::Geometry(placed),
            "the box moves where placed"
        );
        assert_eq!(crop.left, 2);

        let at = super::Endpoint {
            geometry: canvas.around(&placed).expect("around"),
            crop: Crop::NONE,
            opacity: 1.0,
            blend: BlendMode::Normal,
        };
        let layer = super::moved_track(true, &by, at);
        assert_eq!(layer.geometry, at.geometry, "the canvas layer stays put");
        assert_eq!(layer.opacity, 0.5, "and takes the motion's opacity");
        let track = super::moved_track(false, &by, at);
        assert_eq!(
            track.geometry.center.x.get(),
            at.geometry.center.x.get() + 40.0
        );
    }

    fn placed(source: SourceRef, placement: Placement) -> Placed {
        Placed {
            item: ItemId::new("itm"),
            through: None,
            source,
            placement,
            crop: Crop::NONE,
            opacity: super::OPAQUE,
            blend: jstream_protocol::BlendMode::Normal,
            effects: None,
            moving: None,
            motion: crate::motion::TrackMotion::same(ItemId::new("itm"), std::sync::Arc::from([])),
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

    /// A 3D scene's pass carries its camera, and a keyframe scene follows its parent's
    /// unless it overrides it; a live camera wins, on the keyframe's own scene or on the
    /// parent it follows (ADR-0094 §6).
    #[test]
    fn a_pass_draws_through_its_scenes_camera_and_live_cameras_win() {
        use super::{FilterAssets, LiveParams, plan_scene};
        use jstream_graph::Graph;
        use jstream_protocol::{Canvas, Edit, SceneCamera, Vec3};

        let framing = SceneCamera::framing(Canvas::DEFAULT);
        let moved = |x: f32| SceneCamera {
            position: Vec3::new(x, 100.0, 1400.0).expect("finite"),
            ..framing
        };
        let set = |scene: &str, camera: SceneCamera| Edit::SceneSetCamera {
            scene_id: SceneId::new(scene),
            camera: Some(camera),
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
                        scene_id: SceneId::new("flat"),
                        name: "Flat".into(),
                        role: None,
                    },
                    Edit::SceneCreateVariant {
                        scene_id: SceneId::new("k"),
                        variant_of: SceneId::new("main"),
                        name: "Follows".into(),
                    },
                    Edit::SceneCreateVariant {
                        scene_id: SceneId::new("k2"),
                        variant_of: SceneId::new("main"),
                        name: "Overrides".into(),
                    },
                    Edit::SceneCreateVariant {
                        scene_id: SceneId::new("kf"),
                        variant_of: SceneId::new("flat"),
                        name: "Flat keyframe".into(),
                    },
                    set("main", framing),
                    set("k2", moved(200.0)),
                ],
                None,
            )
            .expect("edits");
        let seen = |live: &LiveParams, program: &str| {
            let planned = plan_scene(
                &graph,
                &SceneId::new(program),
                &mut |_| None,
                &FilterAssets::default(),
            );
            live.camera(planned.shot.passes.last().expect("a pass"))
        };
        let none = LiveParams::default();
        assert_eq!(seen(&none, "main"), Some(framing));
        assert_eq!(seen(&none, "k"), Some(framing));
        assert_eq!(seen(&none, "k2"), Some(moved(200.0)));
        assert_eq!(seen(&none, "flat"), None);

        let live_main = set("main", moved(500.0));
        let live = LiveParams::from_values([(None, &live_main)]);
        assert_eq!(seen(&live, "main"), Some(moved(500.0)));
        assert_eq!(seen(&live, "k"), Some(moved(500.0)), "it follows");
        assert_eq!(seen(&live, "k2"), Some(moved(200.0)), "it overrides");
        let live_k2 = set("k2", moved(700.0));
        let live = LiveParams::from_values([(None, &live_main), (None, &live_k2)]);
        assert_eq!(seen(&live, "k2"), Some(moved(700.0)));

        // A live removal of a keyframe's override falls back to its parent's camera.
        let unset = |scene: &str| Edit::SceneSetCamera {
            scene_id: SceneId::new(scene),
            camera: None,
        };
        let drop_k2 = unset("k2");
        let live = LiveParams::from_values([(None, &drop_k2)]);
        assert_eq!(seen(&live, "k2"), Some(framing), "back to the parent's");
        let live = LiveParams::from_values([(None, &live_main), (None, &drop_k2)]);
        assert_eq!(seen(&live, "k2"), Some(moved(500.0)));
        // A live removal of the parent's camera makes every keyframe 2D, overrides too.
        let drop_main = unset("main");
        let live = LiveParams::from_values([(None, &drop_main)]);
        assert_eq!(seen(&live, "main"), None);
        assert_eq!(seen(&live, "k"), None);
        assert_eq!(seen(&live, "k2"), None, "a 2D parent's keyframes are 2D");
        // A live camera on a 2D parent reaches its keyframes.
        assert_eq!(seen(&none, "kf"), None);
        let live_flat = set("flat", moved(300.0));
        let live = LiveParams::from_values([(None, &live_flat)]);
        assert_eq!(seen(&live, "flat"), Some(moved(300.0)));
        assert_eq!(seen(&live, "kf"), Some(moved(300.0)));
    }

    /// A Move between two 3D scenes moves the camera with the preset's timing; a Fade
    /// keeps each side's camera on its own tracks (ADR-0094 decision 7).
    #[test]
    fn a_move_moves_the_camera_and_a_fade_keeps_each_sides() {
        use super::Seen;
        use jstream_protocol::{Canvas, Easing, SceneCamera, Vec3};
        use jstream_render::transition::{Effect, Timing, Track};

        let from = SceneCamera::framing(Canvas::DEFAULT);
        let to = SceneCamera {
            position: Vec3::new(1960.0, 540.0, 1304.0).expect("finite"),
            ..from
        };
        let at = |x: f32| super::Endpoint {
            geometry: jstream_protocol::ItemGeometry::try_new(
                jstream_protocol::Vec2::new(x, 100.0).expect("finite"),
                jstream_protocol::Size::try_new(100.0, 100.0).expect("size"),
            )
            .expect("geometry"),
            crop: Crop::NONE,
            opacity: 1.0,
            blend: BlendMode::Normal,
        };
        let timing = Timing {
            delay_ms: 0.0,
            duration_ms: 1000.0,
            easing: Easing::Linear,
        };
        let leaving = Track::Disappear {
            from: at(0.0),
            effect: Effect::Fade,
            timing,
        };
        let coming = Track::Appear {
            to: at(0.0),
            effect: Effect::Fade,
            timing,
        };
        let mut cameras = super::TrackCameras {
            ends: (Some(from), Some(to)),
            overlay: None,
            timing,
            pairs: true,
            overlay_run: 1,
        };
        let planned = Seen::AsPlanned;
        let halfway = cameras.camera(&leaving, planned, 500.0).expect("3D");
        assert_eq!(halfway.position.x.get(), 1460.0);
        assert_eq!(cameras.camera(&coming, planned, 1000.0), Some(to));
        cameras.pairs = false;
        assert_eq!(cameras.camera(&leaving, planned, 500.0), Some(from));
        assert_eq!(cameras.camera(&coming, planned, 500.0), Some(to));
        cameras.ends = (Some(from), None);
        assert_eq!(cameras.camera(&coming, planned, 500.0), None);
        // A stinger's items are seen through their own scene's camera, or none.
        let through = Track::Through {
            at: at(0.0),
            appear: Effect::Fade,
            enter: timing,
            disappear: Effect::Fade,
            leave: timing,
        };
        cameras.ends = (Some(from), Some(to));
        assert_eq!(
            cameras.camera(&through, planned, 500.0),
            None,
            "a 2D stinger scene"
        );
        cameras.overlay = Some(from);
        assert_eq!(cameras.camera(&through, planned, 500.0), Some(from));
        // Outside a Move, what a switch took from the screen leaves through the camera
        // it had then, 2D included, whatever the ends.
        let held = |camera| Seen::Held {
            camera,
            depth_run: 0,
        };
        assert_eq!(cameras.camera(&leaving, held(Some(to)), 500.0), Some(to));
        assert_eq!(cameras.camera(&leaving, held(None), 500.0), None);
        // A Move starts from a screen seen through one camera, its first end, and
        // moves it for every track, leaving ones too.
        cameras.pairs = true;
        let moving = cameras.camera(&leaving, held(Some(from)), 500.0);
        assert_eq!(moving.map(|camera| camera.position.x.get()), Some(1460.0));
    }

    /// A scene's background is planned with it, a keyframe scene draws its parent's and
    /// a group none (ADR-0089 §4); a transition fades from one background to the other.
    #[test]
    fn a_keyframe_draws_its_parents_background_and_a_transition_fades_it() {
        use super::{FilterAssets, Transitioning, plan_scene};
        use jstream_graph::Graph;
        use jstream_protocol::{Edit, SceneOptions};
        use jstream_render::Color;

        let mut graph = Graph::default();
        graph
            .apply(
                &[
                    Edit::SceneCreate {
                        scene_id: SceneId::new("main"),
                        name: "Main".into(),
                        role: None,
                    },
                    Edit::SceneCreateVariant {
                        scene_id: SceneId::new("k"),
                        variant_of: SceneId::new("main"),
                        name: "Keyframe".into(),
                    },
                    Edit::SceneSetOptions {
                        scene_id: SceneId::new("main"),
                        options: SceneOptions {
                            background: Some("#ff000080".into()),
                            ..SceneOptions::default()
                        },
                    },
                ],
                None,
            )
            .expect("edits");
        graph
            .apply(
                &[
                    Edit::SceneCreate {
                        scene_id: SceneId::new("g"),
                        name: "Group".into(),
                        role: Some(jstream_protocol::SceneRole::Group),
                    },
                    Edit::SceneSetOptions {
                        scene_id: SceneId::new("g"),
                        options: SceneOptions {
                            background: Some("#00ff00".into()),
                            ..SceneOptions::default()
                        },
                    },
                ],
                None,
            )
            .expect("a group");
        let group = plan_scene(
            &graph,
            &SceneId::new("g"),
            &mut |_| None,
            &FilterAssets::default(),
        );
        let pass = group.shot.passes.last().map(|pass| pass.background);
        assert_eq!(pass, Some(Color::TRANSPARENT), "a group has none");
        let half_red = Color::from_rgba8([255, 0, 0, 128]);
        for program in ["main", "k"] {
            let planned = plan_scene(
                &graph,
                &SceneId::new(program),
                &mut |_| None,
                &FilterAssets::default(),
            );
            let pass = planned.shot.passes.last().map(|pass| pass.background);
            assert_eq!(pass, Some(half_red), "{program}");
        }

        let fading = Transitioning {
            start: zero(),
            tracks: Vec::new(),
            backgrounds: (Color::BLACK, Color::rgb(1.0, 0.0, 0.0)),
            cameras: super::TrackCameras {
                ends: (None, None),
                overlay: None,
                timing: jstream_render::transition::Timing {
                    delay_ms: 0.0,
                    duration_ms: 1000.0,
                    easing: jstream_protocol::Easing::Linear,
                },
                pairs: false,
                overlay_run: 1,
            },
            moves: (None, None),
            blends: Vec::new(),
            end_ms: 1000.0,
        };
        let at = |ms: i64| MediaTime::new(ms, Timebase::new(1, 1000).expect("valid"));
        assert_eq!(fading.background(zero()), Color::BLACK);
        assert_eq!(fading.background(at(1000)), Color::rgb(1.0, 0.0, 0.0));
        let middle = fading.background(at(500));
        assert!(
            middle.r > 0.5 && middle.r < 1.0 && middle.a == 1.0,
            "{middle:?}"
        );
    }

    /// A previewed source is planned on a copy of the collection made once per
    /// revision: planning it again at the same revision reuses the copy, and an edit
    /// makes a new one that shows the edit (#969 N2).
    #[test]
    fn a_source_preview_copies_the_collection_once_per_revision() {
        use jstream_graph::Graph;
        use jstream_protocol::{Edit, SourceKind};

        let cam = SourceId::new("cam");
        let mut graph = Graph::new().with_kinds(crate::kinds::builtin());
        graph
            .apply(
                &[Edit::SourceCreate {
                    source_id: cam.clone(),
                    name: "Camera".into(),
                    kind: SourceKind::Builtin {
                        kind: "display".into(),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                }],
                None,
            )
            .expect("edits");
        let mut copies = super::SourceCopies::default();
        let plan = |copies: &mut super::SourceCopies, graph: &Graph| {
            super::plan_source(
                copies,
                graph,
                &cam,
                &mut |id| {
                    Some(super::Look::Capture(CaptureKey::for_source(
                        CaptureSpec::Display(None),
                        id.clone(),
                    )))
                },
                &crate::filter_assets::FilterAssets::default(),
            )
            .expect("the source is planned")
            .shot
        };
        let first = plan(&mut copies, &graph);
        assert_eq!(copies.built, 1);
        assert_eq!(
            first.program().map(SceneId::as_str),
            Some(super::SOURCE_SCENE)
        );
        let again = plan(&mut copies, &graph);
        assert_eq!(copies.built, 1, "the same revision reuses the copy");
        assert_eq!(again, first);

        graph
            .apply(
                &[Edit::CanvasSetSize {
                    width: 640,
                    height: 360,
                }],
                None,
            )
            .expect("canvas");
        let resized = plan(&mut copies, &graph);
        assert_eq!(copies.built, 2, "an edit makes a new copy");
        assert_eq!((resized.canvas.width, resized.canvas.height), (640, 360));
        let placed = resized
            .placed(&SceneId::new(super::SOURCE_SCENE))
            .expect("the source's scene");
        assert_eq!(placed.len(), 1, "one item: {placed:?}");
        let again = plan(&mut copies, &graph);
        assert_eq!(copies.built, 2);
        assert_eq!(again, resized);

        // A source no longer previewed lets its copy go.
        copies.retain(&BTreeSet::new());
        plan(&mut copies, &graph);
        assert_eq!(copies.built, 3);
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

    /// A group a mix nests that a `scene` source's chain also reads, at the same depth,
    /// is drawn once a frame (#1070): the mix reads the layer the chain phase drew, and
    /// shows it both as itself and through the chain.
    #[test]
    fn a_nested_scene_a_chain_also_reads_is_drawn_once() {
        use jstream_graph::Graph;
        use serde_json::json;

        let geometry = |cx: f32, cy: f32, w: f32, h: f32| json!({"center": [cx, cy], "size": [w, h], "fit_mode": "stretch"});
        let edits = [
            json!({"type": "scene.create", "scene_id": "main", "name": "Main"}),
            json!({"type": "scene.create", "scene_id": "g", "name": "G", "role": "group"}),
            json!({"type": "source.create", "source_id": "green", "name": "Green",
                   "kind": {"type": "builtin", "kind": "image"}}),
            json!({"type": "item.add", "scene_id": "g", "item_id": "green", "source_id": "green",
                   "geometry": geometry(480.0, 270.0, 960.0, 540.0)}),
            json!({"type": "source.create", "source_id": "as-is", "name": "As is",
                   "kind": {"type": "scene", "scene_id": "g"}}),
            json!({"type": "source.create", "source_id": "blurred", "name": "Blurred",
                   "kind": {"type": "scene", "scene_id": "g"}}),
            json!({"type": "filter.add", "owner": {"type": "source", "source_id": "blurred"},
                   "chain": "video", "filter_id": "b", "kind": "blur",
                   "settings": {"radius": 2.0}}),
            json!({"type": "item.add", "scene_id": "main", "item_id": "as-is",
                   "source_id": "as-is", "geometry": geometry(960.0, 540.0, 1920.0, 1080.0)}),
            json!({"type": "item.add", "scene_id": "main", "item_id": "blurred",
                   "source_id": "blurred",
                   "geometry": geometry(1440.0, 810.0, 960.0, 540.0)}),
        ]
        .into_iter()
        .map(|edit| serde_json::from_value::<Edit>(edit).expect("an edit"))
        .collect::<Vec<_>>();
        let mut graph = Graph::new().with_kinds(crate::kinds::builtin());
        graph.apply(&edits, None).expect("edits");
        let green = Frame::new(1, 1, &GREEN, 1).expect("a pixel");
        let frames = BTreeMap::from([(SourceId::new("green"), green)]);
        let shot = super::plan(&graph, &frames);
        let layer = GpuKey::Layer(SceneId::new("g"), 1);
        assert!(
            shot.passes.iter().any(|pass| pass.key == layer),
            "the mix nests g at depth 1"
        );

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Ok(mut compositor) = Compositor::open(OpenOptions::hardware()) else {
            assert!(std::env::var_os("CI").is_none(), "CI has no wgpu adapter");
            return;
        };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let canvas = shot.canvas().expect("a canvas");
        cache.begin_frame();
        let output = compose(
            &mut compositor,
            (&mut cache, &mut chains),
            (
                &shot,
                MediaTime::new(0, Timebase::new(1, 1000).expect("valid")),
            ),
            View::default(),
            &LiveParams::default(),
            &super::NoCaptures,
            super::DrawAt::read(canvas),
        )
        .expect("compose");
        drop(cache.end_frame(&compositor));
        assert_eq!(
            chains.reused.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the mix read g's layer instead of drawing it again"
        );
        let frame = output.pixels().expect("read back");
        let at = |x: usize, y: usize| {
            let i = (y * frame.width() as usize + x) * 4;
            let rgba = frame.rgba();
            [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
        };
        // g as itself: green over its top-left quarter, black beside it.
        assert_eq!(at(480, 270), GREEN);
        assert_eq!(at(1440, 270), [0, 0, 0, 255]);
        // Through the blur, scaled into the bottom-right quarter: green in the middle
        // of its own top-left quarter.
        assert_eq!(at(1200, 675), GREEN);
        assert_eq!(at(1700, 950), [0, 0, 0, 255]);
    }

    /// A scene's background fills its canvas under its items wherever it is drawn
    /// (ADR-0024 §4): the nested `a`'s red fills the quarter it is placed at, over the
    /// program's blue. A background with alpha shows over black where a mix shows it.
    #[test]
    fn a_scene_draws_its_background_nested_or_not() {
        use jstream_render::Color;
        let quarter = Placement::Legacy(LegacyTransform {
            scale: [0.5, 0.5],
            ..LegacyTransform::IDENTITY
        });
        let mut shot = shot(vec![("program", vec![nest("a", quarter)]), ("a", vec![])]);
        let blue = Color::rgb(0.0, 0.0, 1.0);
        shot.backgrounds.insert(SceneId::new("program"), blue);
        shot.backgrounds
            .insert(SceneId::new("a"), Color::rgb(1.0, 0.0, 0.0));
        shot.passes = schedule(&shot, &SceneId::new("program"));
        let Some(rgba) = snap(&shot) else { return };
        assert_eq!(at(&rgba, 10, 10), RED);
        assert_eq!(at(&rgba, 630, 350), RED);
        for (x, y) in [(650, 10), (10, 370), (1270, 710)] {
            assert_eq!(at(&rgba, x, y), [0, 0, 255, 255], "({x}, {y})");
        }
        // Half blue over black is 0.5 in linear light, sRGB 188.
        shot.backgrounds
            .insert(SceneId::new("program"), Color { a: 0.5, ..blue });
        shot.passes = schedule(&shot, &SceneId::new("program"));
        let Some(rgba) = snap(&shot) else { return };
        let [r, g, b, a] = at(&rgba, 1000, 600);
        assert_eq!((r, g, a), (0, 0, 255));
        assert!(b.abs_diff(188) <= 1, "{b}");
    }

    /// An item's opacity multiplies its alpha, and the result blends in the collection's
    /// space (ADR-0126): half green over red is 0.5 of each in linear light, sRGB 188,
    /// and 0.5 of each encoded value browser-style, sRGB 128. A nested scene's opacity
    /// applies to the scene as a whole, and an item at 0 draws nothing.
    #[test]
    fn opacity_blends_items_and_nested_scenes_in_the_blending_space() {
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
        let mut shot = shot(vec![
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
        for (blending, half) in [
            (jstream_protocol::Blending::Linear, [188, 188, 0, 255]),
            (jstream_protocol::Blending::Srgb, [128, 128, 0, 255]),
        ] {
            shot.blending = blending;
            let Some(rgba) = snap(&shot) else { return };
            for (x, y) in [(100, 360), (1100, 360)] {
                let pixel = at(&rgba, x, y);
                for (got, want) in pixel.iter().zip(half) {
                    assert!(
                        got.abs_diff(want) <= 1,
                        "{blending:?} ({x}, {y}): {pixel:?}"
                    );
                }
            }
        }
    }

    /// Each item draws with its blend mode (ADR-0082): additive green over red is yellow,
    /// and a nested scene's items blend with its own layer, so its difference green over
    /// the layer's transparent background stays green, which the layer then multiplies
    /// onto the red, making black.
    #[test]
    fn items_and_nested_scenes_draw_their_blend_modes() {
        let blended = |placed: Placed, blend: BlendMode| Placed { blend, ..placed };
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
                    blended(
                        placed(SourceRef::Frame(SourceId::new("green")), left),
                        BlendMode::Additive,
                    ),
                    blended(nest("a", right), BlendMode::Multiply),
                ],
            ),
            ("a", vec![blended(fill("green"), BlendMode::Difference)]),
        ]);
        let Some(rgba) = snap(&shot) else { return };
        for ((x, y), want) in [
            ((100, 360), [255, 255, 0, 255]),
            ((1100, 360), [0, 0, 0, 255]),
        ] {
            let pixel = at(&rgba, x, y);
            for (got, want) in pixel.iter().zip(want) {
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

    /// A metered frame names each unit it ran: the uploads, the nested scene's pass
    /// and the program's composite, and its phases add up to no more than the frame
    /// (ADR-0103).
    #[test]
    fn a_metered_frame_names_its_units() {
        use super::{DrawAt, NoCaptures, compose_metered};
        use crate::profile::UnitKey;
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let right = Placement::Legacy(LegacyTransform {
            position: [960.0, 0.0],
            scale: [0.5, 1.0],
            rotation_deg: 0.0,
        });
        let shot = shot(vec![
            ("program", vec![fill("red"), nest("a", right)]),
            ("a", vec![fill("green")]),
        ]);
        let slot = Arc::new(crate::profile::DrawerSlot::default());
        let mut meter = slot.meter(Duration::from_millis(33));
        let mut cache = ResourceCache::new();
        let started = Instant::now();
        cache.begin_frame();
        let output = compose_metered(
            &mut compositor,
            (&mut cache, &mut Chains::default(), &mut meter),
            (&shot, zero()),
            View::default(),
            &LiveParams::default(),
            &NoCaptures,
            DrawAt::read(FrameSize::new(1280, 720).expect("size")),
        );
        drop(cache.end_frame(&compositor));
        output.expect("drawn");
        let took = started.elapsed();
        // The first frame starts the second; one a second later publishes it.
        meter.end_frame(took, started, drop);
        assert!(slot.load().is_none(), "nothing published within the second");
        meter.end_frame(took, started + Duration::from_secs(1), drop);
        let totals = slot.load().expect("published");
        assert_eq!(totals.frames, 2);
        let units: Vec<_> = totals.units.iter().map(|(key, _)| key.clone()).collect();
        for want in [
            UnitKey::Upload(SourceId::new("green")),
            UnitKey::Upload(SourceId::new("red")),
            UnitKey::Pass(SceneId::new("a")),
            UnitKey::Composite,
        ] {
            assert!(units.contains(&want), "{want:?} in {units:?}");
        }
        let (_, pass) = totals
            .units
            .iter()
            .find(|(key, _)| *key == UnitKey::Pass(SceneId::new("a")))
            .expect("the pass");
        assert_eq!(pass.frames, 1, "only the drawn frame ran it");
        let phases: u64 = totals.phases.iter().sum();
        assert!(phases > 0, "the phases were timed");
        assert!(
            u128::from(phases) <= took.as_nanos(),
            "the phases fit in the frame: {phases} > {took:?}"
        );
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

    /// A layer a transition moves inside a nested scene (ADR-0049 §3) is drawn where its
    /// track is at the frame's time: the nested pass samples it, as the program's
    /// tracks are.
    #[test]
    fn a_moving_layer_is_drawn_where_its_track_is() {
        use jstream_render::transition::{Endpoint, Timing, Track};
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let size = FrameSize::new(32, 18).expect("invariant: non-zero");
        let half = |x: f32| Endpoint {
            geometry: jstream_protocol::ItemGeometry::try_new(
                jstream_protocol::Vec2::new(x, 540.0).expect("finite"),
                jstream_protocol::Size::try_new(960.0, 1080.0).expect("positive"),
            )
            .expect("valid"),
            crop: Crop::NONE,
            opacity: super::OPAQUE,
            blend: jstream_protocol::BlendMode::Normal,
        };
        let track = Track::Pair {
            from: half(480.0),
            to: half(1440.0),
            timing: Timing {
                delay_ms: 0.0,
                duration_ms: 1000.0,
                easing: jstream_protocol::Easing::Linear,
            },
            source: None,
        };
        let moving = Placed {
            moving: Some((zero(), track)),
            ..fill("green")
        };
        let mix = shot(vec![
            (
                "program",
                vec![fill("red"), nest("inner", Placement::IDENTITY)],
            ),
            ("inner", vec![moving]),
        ]);
        let mut at = |ms: i64| {
            cache.begin_frame();
            let output = compose(
                &mut compositor,
                (&mut cache, &mut Chains::default()),
                (
                    &mix,
                    MediaTime::new(ms, Timebase::new(1, 1000).expect("valid")),
                ),
                View::default(),
                &LiveParams::default(),
                &Every(None),
                super::DrawAt::read(size),
            )
            .expect("compose");
            drop(cache.end_frame(&compositor));
            output.pixels().expect("invariant: read back").clone()
        };
        let start = at(0);
        assert_eq!(start.pixel(8, 9), Some(GREEN), "the start: on the left");
        assert_eq!(start.pixel(24, 9), Some(RED));
        let end = at(1000);
        assert_eq!(end.pixel(8, 9), Some(RED));
        assert_eq!(end.pixel(24, 9), Some(GREEN), "the end: on the right");
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
        let live = LiveParams::from_values([(None, &half)]);
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
        let live = LiveParams::from_values([(None, &cropped)]);
        let out = live_frame_at(&mut compositor, &mut cache, &mix, &live, None, size);
        assert_eq!(out.pixel(8, 9), Some(GREEN));
        assert_eq!(out.pixel(24, 9), Some(RED), "cropped away on the right");
    }

    /// Where a keyframe scene draws an item, its own live crop wins; the item's own live
    /// crop shows only while the keyframe doesn't override the crop (#846).
    #[test]
    fn a_keyframe_takes_the_items_live_crop_only_where_it_follows() {
        let item = ItemId::new("top");
        let cut = |right: u32| Edit::ItemSetCrop {
            item_id: item.clone(),
            scene_id: None,
            crop: jstream_protocol::Crop {
                right,
                ..jstream_protocol::Crop::NONE
            },
        };
        let (own, keyed) = (cut(4), cut(8));
        let k = SceneId::new("k");
        let through = |crop: bool| super::Through {
            keyframe: k.clone(),
            geometry: false,
            crop,
        };
        let right = |live: &LiveParams, through: Option<&super::Through>| {
            live.apply_to(&item, through, FULL, Crop::NONE).1.right
        };
        let live = LiveParams::from_values([(None, &own)]);
        assert_eq!(right(&live, None), 4);
        assert_eq!(right(&live, Some(&through(false))), 4, "follows the item");
        assert_eq!(
            right(&live, Some(&through(true))),
            0,
            "overridden: the shot's"
        );
        let live = LiveParams::from_values([(None, &own), (Some(&k), &keyed)]);
        assert_eq!(right(&live, None), 4, "the keyframe's isn't the item's");
        assert_eq!(right(&live, Some(&through(true))), 8);
        let other = super::Through {
            keyframe: SceneId::new("other"),
            geometry: false,
            crop: true,
        };
        assert_eq!(
            right(&live, Some(&other)),
            0,
            "another keyframe's isn't its"
        );
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

    /// A still only items draw is held at the size they draw it, on a cache that allows
    /// it, from its first upload, and still shows its colour (#1139). One a video chain
    /// reads, or one standing in for a model, is not.
    #[test]
    fn a_still_drawn_small_is_uploaded_small() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        cache.draw_uploads_small(true);
        let size = FrameSize::new(1920, 1080).expect("invariant: non-zero");
        let bytes: Vec<u8> = std::iter::repeat_n(GREEN, 1024 * 512).flatten().collect();
        let big = Frame::new(1024, 512, &bytes, 3).expect("invariant: 1024x512 RGBA8");
        // A 1024x512 frame in a 128x64 box: an eighth each way, held at a quarter.
        let small = Placement::Geometry(jstream_protocol::ItemGeometry {
            size: jstream_protocol::Size::try_new(128.0, 64.0).expect("positive"),
            ..jstream_protocol::ItemGeometry::covering_canvas(1920, 1080).expect("positive")
        });
        let mut drawn = shot(vec![(
            "program",
            vec![
                fill("red"),
                placed(SourceRef::Frame(SourceId::new("big")), small),
            ],
        )]);
        drawn.frames.insert(SourceId::new("big"), big);
        drawn
            .uploads
            .push((GpuKey::Frame(SourceId::new("big")), SourceId::new("big")));
        let key = GpuKey::Frame(SourceId::new("big"));
        for _ in 0..2 {
            let out = frame_at(&mut compositor, &mut cache, &drawn, None, size);
            assert_eq!(out.pixel(960, 540), Some(GREEN), "the still's centre");
            let image = cache.get(&key).expect("uploaded");
            assert_eq!(
                (image.width(), image.height(), image.extent()),
                (256, 128, (1024, 512))
            );
        }
        let frame = drawn.frames[&SourceId::new("big")].clone();
        assert!(drawn.drawn_only(&key, &frame));
        drawn.chains.insert(
            ChainOwner::Source(SourceId::new("big")),
            Chain {
                input: key.clone(),
                filters: Vec::new(),
                pad: None,
            },
        );
        assert!(!drawn.drawn_only(&key, &frame), "a chain reads its texels");
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
                pad: None,
            },
        );
        assert_eq!(scales(&planned)[1], (SourceId::new("red"), 1.0));
    }

    /// #1168: a legacy item counts only when `legacy_quad` draws it. A scale of `1e35`
    /// on a 4096-wide frame overflows and counts as 0, and so does a corner that
    /// overflows after a turn or a shift to `f32::MAX`, and a crop that leaves nothing.
    /// The same `1e35` on a 1×1 frame still draws, and a scale that stays finite still
    /// counts.
    #[test]
    fn a_legacy_scale_that_draws_nothing_is_not_magnified() {
        let legacy = |scale: [f32; 2], rotation_deg: f32, position: [f32; 2]| {
            Placement::Legacy(LegacyTransform {
                position,
                scale,
                rotation_deg,
            })
        };
        let wide = frame(RED).placed_at(FrameSize::new(4096, 2160).expect("non-zero"));
        let tiny = frame(GREEN).placed_at(FrameSize::new(1, 1).expect("non-zero"));
        let mut cut = placed(
            SourceRef::Frame(SourceId::new("cut")),
            legacy([4.0, 4.0], 0.0, [0.0, 0.0]),
        );
        cut.crop = Crop {
            left: 4096,
            ..Crop::NONE
        };
        let mut planned = shot(vec![(
            "program",
            vec![
                placed(
                    SourceRef::Frame(SourceId::new("overflow")),
                    legacy([1e35, 1.0], 0.0, [0.0, 0.0]),
                ),
                placed(
                    SourceRef::Frame(SourceId::new("turned")),
                    legacy([8e34, -1.5e35], 45.0, [0.0, 0.0]),
                ),
                placed(
                    SourceRef::Frame(SourceId::new("shifted")),
                    legacy([1e34, 1.0], 0.0, [f32::MAX, 0.0]),
                ),
                placed(
                    SourceRef::Frame(SourceId::new("tiny")),
                    legacy([1e35, 1.0], 0.0, [0.0, 0.0]),
                ),
                placed(
                    SourceRef::Frame(SourceId::new("kept")),
                    legacy([3.0, -1.5], 0.0, [0.0, 0.0]),
                ),
                cut,
            ],
        )]);
        planned
            .frames
            .insert(SourceId::new("overflow"), wide.clone());
        planned.frames.insert(SourceId::new("turned"), wide.clone());
        planned
            .frames
            .insert(SourceId::new("shifted"), wide.clone());
        planned.frames.insert(SourceId::new("cut"), wide);
        planned.frames.insert(SourceId::new("tiny"), tiny);
        planned.frames.insert(SourceId::new("kept"), frame(GREEN));
        let scale = |id: &str| {
            planned
                .scales()
                .get(&SourceId::new(id))
                .copied()
                .unwrap_or(f32::NAN)
        };
        assert_eq!(scale("overflow"), 0.0, "the size overflows");
        assert_eq!(scale("turned"), 0.0, "a corner overflows after the turn");
        assert_eq!(scale("shifted"), 0.0, "a corner overflows past f32::MAX");
        assert_eq!(scale("cut"), 0.0, "the crop leaves nothing");
        assert_eq!(scale("tiny"), 1e35, "the same scale still draws on 1x1");
        assert_eq!(scale("kept"), 3.0, "a finite scale still counts");
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
                View::mix(mix),
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
        page_shot.blending = jstream_protocol::Blending::Linear;
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
                    View::mix(Some(&half)),
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
                    View::mix(Some(&half)),
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

    /// #1073: no adapter is `action_failed`, and on Linux names the driver to install.
    #[test]
    fn no_adapter_is_action_failed_and_names_the_driver() {
        let error = super::from_render(RenderError::NoAdapter {
            detail: "vulkan drivers/libraries could not be loaded".into(),
        });
        let ProtocolError::ActionFailed { reason } = &error else {
            panic!("{error:?}");
        };
        assert!(
            reason.starts_with("no wgpu adapter: vulkan drivers"),
            "{reason}"
        );
        assert_eq!(
            reason.contains("mesa-vulkan-drivers"),
            cfg!(target_os = "linux"),
            "{reason}"
        );
        assert!(is_no_adapter(&error));
        // Any other render failure is still the engine's.
        assert!(matches!(
            super::from_render(RenderError::Device {
                detail: "lost".into()
            }),
            ProtocolError::Internal { .. }
        ));
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
