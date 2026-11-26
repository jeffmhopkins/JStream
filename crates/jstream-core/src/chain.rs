//! Video filter chains (ADR-0026, roadmap V0): a source's enabled video filters, run in
//! order over its frame before it is placed, with `jstream-render`'s built-in filter
//! kinds and the loaded effect packs' kinds ([`crate::effect_packs`]). A scene's enabled video filters (ADR-0065) run the same way, over the scene's
//! composite ([`GpuKey::Composite`]), once per frame wherever the scene shows; each
//! chain is keyed by its [`ChainOwner`]. The master stage's video chain runs over each
//! mix an output draws, once per frame, and an output's own over what the master stage
//! made of its mix (ADR-0080), both at the canvas size ([`GpuKey::Mixed`]).
//!
//! [`plan`](crate::snapshot::plan_program) copies each reached source's chain into the
//! [`Shot`](crate::snapshot::Shot) as a [`Chain`]: its input (the source's frame upload,
//! or the display capture) and the filters that run, each with its settings. A layer
//! that places the source then shows the chain's output instead of the input.
//!
//! Each compositor that draws shots keeps a [`Chains`]: one [`VideoFilter`] per filter,
//! made when the filter first draws and kept while the shot keeps it, so a stateful kind
//! (a video delay) sees every frame in order, and a settings change updates it in place.
//! A kind that reads an asset is made again, with the new image, when a new load of
//! that asset arrives, and a `scene_mask` when the layer of the scene it reads is
//! remade. Each chain runs after the passes of the scenes it reads, in the order the
//! shot schedules (`snapshot::schedule_chains`).
//! Each filter draws into a layer of the compositor's [`ResourceCache`], under
//! [`GpuKey::Chain`], with the last one under [`GpuKey::Filtered`], so a steady chain
//! allocates no output texture. A filter that no shot keeps is retired into the cache
//! ([`ResourceCache::retire_filter`]), which hands it back with the images it reclaims
//! once the GPU is done with it, so the render thread drops it on its collector
//! (R-RT-003).
//!
//! A shader source of an effect pack (ADR-0093) draws its picture as the first filter of
//! its chain: its program, made from the pack's [`PackSource`] with the source's own
//! settings, over the stand-in its live source shows ([`crate::pack_source`]). The
//! scenes its `scene` parameters name fill the program's input slots, read from their
//! layers like a `scene_mask`'s, and handed in each time a layer is remade, so the
//! program keeps its feedback. The program restarts when its source shows a new frame:
//! the stand-in makes one each time the program mix shows it again.
//!
//! A chain whose filter fails (its layer can't be made, past the adapter's limit, or a
//! shader the driver refuses fails to draw) draws nothing for that frame. The failure
//! is logged once per filter until a frame it draws, so a filter that keeps failing
//! doesn't log every tick. Unlike an audio filter, which ADR-0026 bypasses when it
//! fails, a failed video chain doesn't show its unfiltered input: a key or a mask that
//! fails would otherwise reveal what it hides.
//!
//! Docs: crates/jstream-core/README.md#design, docs/adr/0026-source-filter-chains.md

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use jstream_graph::{Filter, Source};
use jstream_media::{FrameSize, MediaTime};
use jstream_protocol::{
    FilterChain, FilterId, ItemId, OutputId, Reference, SceneId, Settings, SourceId, SourceKind,
    VideoMixId,
};
use jstream_render::{
    Compositor, FilterClock, Image, ProgramAudio, RenderError, ResourceCache, VideoFilter,
};
use jstream_sources::{AudioMeter, AudioScope};

use crate::effect_packs::{PackEffect, PackSource};
use crate::filter_assets::{self, FilterAsset, FilterAssets};
use crate::snapshot::GpuKey;

/// What owns a video chain the engine runs: a source, over its frame, a scene, over its
/// composite, an item, over its box (ADR-0065), or the master stage and an output, over
/// what leaves JStream (ADR-0080).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ChainOwner {
    Source(SourceId),
    Scene(SceneId),
    /// An item's effects, as the scene it names draws the item: a keyframe scene draws
    /// its parent's items at its own values, so each has a chain of its own.
    Item(SceneId, ItemId),
    /// The master stage's chain over one mix an output draws: `None` is the program of
    /// an output that names no mix. Each mix has a chain of its own, so a stateful
    /// filter sees each mix's frames in order.
    Master(Option<VideoMixId>),
    /// An output's own chain, after the master stage.
    Output(OutputId),
    /// A `scene` source's chain where it places its scene (ADR-0065 §4), by the ID the
    /// scene is planned under there: the scene, or a group's view through a keyframe
    /// (ADR-0049 §3). Each has a chain of its own, as each draws its own picture.
    Placed(SourceId, SceneId),
    /// A model source's chain over the layer an item of a scene draws the model into
    /// (ADR-0107 decision 6): one per item that shows it, before the item's effects.
    Shown(SourceId, SceneId, ItemId),
}

impl std::fmt::Display for ChainOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(id) => write!(f, "source {id}"),
            Self::Scene(id) => write!(f, "scene {id}"),
            Self::Item(scene, item) => write!(f, "item {item} in scene {scene}"),
            Self::Master(None) => write!(f, "the master stage over the program"),
            Self::Master(Some(mix)) => write!(f, "the master stage over mix {mix}"),
            Self::Output(id) => write!(f, "output {id}"),
            Self::Placed(id, scene) => write!(f, "source {id} over scene {scene}"),
            Self::Shown(id, scene, item) => {
                write!(f, "source {id} over item {item} in scene {scene}")
            }
        }
    }
}

/// The most `kind` draws beyond each edge of its input at any settings
/// (`max_outset_px`, ADR-0065 §5): 0 for a kind that declares none.
fn max_outset_of(kind: &str) -> u32 {
    static OUTSETS: OnceLock<BTreeMap<String, u32>> = OnceLock::new();
    OUTSETS
        .get_or_init(|| {
            jstream_render::video_filter_descriptors()
                .into_iter()
                .chain(jstream_render::depth_filter_descriptors())
                .filter_map(|kind| kind.max_outset_px.map(|px| (kind.kind_id, px)))
                .collect()
        })
        .get(kind)
        .copied()
        .unwrap_or(0)
}

/// The step an outset is rounded up to, px: a Move that blends a shadow's blur or a
/// glow's radius (ADR-0127) resizes the padded layer only when it crosses a step.
const OUTSET_STEP: u32 = 16;

/// How far a filter of `kind` draws beyond each edge of its input at `settings`
/// (ADR-0136), rounded up to [`OUTSET_STEP`] and never more than the kind's
/// `max_outset_px`: a kind render doesn't measure takes that whole.
fn outset_of(kind: &str, settings: &Settings) -> u32 {
    let most = max_outset_of(kind);
    if most == 0 {
        return 0;
    }
    match jstream_render::video_filter_outset(kind, settings) {
        Some(px) => ((px.max(0.0).ceil() as u32).div_ceil(OUTSET_STEP) * OUTSET_STEP).min(most),
        None => most,
    }
}

/// The `int` parameters of `descriptor`, which a Move's blend keeps whole (ADR-0127).
fn whole_params(descriptor: &jstream_protocol::KindDescriptor) -> Vec<String> {
    descriptor
        .params
        .iter()
        .filter(|param| matches!(param.ty, jstream_protocol::ParamType::Int { .. }))
        .map(|param| param.id.clone())
        .collect()
}

/// One filter of a [`Chain`], as the collection holds it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChainFilter {
    pub(crate) id: FilterId,
    pub(crate) kind: String,
    pub(crate) settings: Settings,
    /// The asset it reads (`apply_lut`, `image_mask`), once loaded. `None` for a kind
    /// that reads none, or while its asset isn't loaded: it then passes its input
    /// through.
    pub(crate) reference: Option<Arc<FilterAsset>>,
    /// The layer of the scene it reads (`scene_mask`), [`GpuKey::reference`] of the
    /// scene its setting names. `None` for a kind that reads none, or a `null` setting:
    /// it then passes its input through.
    pub(crate) scene: Option<GpuKey>,
    /// The effect that draws it, for a kind from an effect pack; `None` for a built-in
    /// kind.
    pub(crate) effect: Option<PackEffect>,
    /// The shader source whose program it is, for the first filter of a pack source's
    /// chain (ADR-0093); `None` for any other filter.
    pub(crate) program: Option<Program>,
}

/// The program that starts a pack source's chain, as a [`ChainFilter`] holds it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Program {
    pub(crate) source: PackSource,
    /// The layer of the scene each of its `scene` parameters names, in slot order
    /// ([`GpuKey::reference`]): `None` for a `null` one, which reads transparent.
    pub(crate) slots: Vec<Option<GpuKey>>,
    /// The version of the frame its source showed when planned: a new one restarts it.
    pub(crate) shown: u64,
    /// The scope it hears, for a shader source that hears audio: measured each frame
    /// it draws.
    pub(crate) heard: Option<Heard>,
}

/// The audio scope a program hears. Two are equal when they are the same scope.
#[derive(Clone)]
pub(crate) struct Heard(pub(crate) Arc<AudioScope>);

impl PartialEq for Heard {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for Heard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Heard")
    }
}

/// The ID of the filter a pack source's program runs as. The kind is the source's, which
/// no filter of the collection has, so it can't match a filter the collection holds.
const PROGRAM_FILTER: &str = "program";

/// What a scene's composite drew beside its view depth for its filters: the scene
/// mirrored in its floor with that picture's depth and the floor's height (ADR-0151
/// decision 4), and its surface buffer (ADR-0170 decision 4), each when drawn.
pub(crate) type DrawnBeside<'k> = (
    Option<(&'k GpuKey, &'k GpuKey, f32)>,
    Option<&'k [GpuKey; 3]>,
);

/// The ID of the screen-space reflection pass a reflective scene's chain starts with
/// (ADR-0170 decision 4). Its kind is no collection filter's, so it can't match one.
const SCREEN_REFLECTIONS_FILTER: &str = "screen_space_reflections";

impl ChainFilter {
    /// A render filter drawing it: its shader source's program, its pack's effect, or
    /// the built-in kind.
    fn video_filter(&self) -> Option<VideoFilter> {
        match &self.program {
            Some(program) => Some(program.source.video_filter(&self.settings)),
            None => make(&self.kind, &self.settings, self.effect.as_ref()),
        }
    }
}

/// A render filter of `kind` with `settings`: drawn by `effect` for a pack kind, else by
/// the built-in kind of that ID. `None` for a kind this engine doesn't draw.
fn make(kind: &str, settings: &Settings, effect: Option<&PackEffect>) -> Option<VideoFilter> {
    match effect {
        Some(effect) => Some(effect.video_filter(settings)),
        None => VideoFilter::new(kind, settings),
    }
}

/// A source's or a scene's video chain as a shot draws it: the image it starts from
/// and its enabled filters of a kind this engine draws, first filter first. Never
/// empty.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Chain {
    pub(crate) input: GpuKey,
    pub(crate) filters: Vec<ChainFilter>,
    /// For an item's effects: the filter whose input is padded, and by how many layer
    /// pixels on every side (ADR-0065 §5, [`Self::pad_at_also`]). `None` for any other chain.
    pub(crate) pad: Option<(usize, u32)>,
}

impl Chain {
    /// `source`'s chain over `input`, or `None` when no filter of it runs: the chain is
    /// empty, every filter is disabled, or none is of a kind this engine draws (an
    /// effect pack that isn't loaded). Such a filter is bypassed. A filter that reads an
    /// asset gets it from `assets`, and a pack kind's effect comes from there too.
    ///
    /// A source of a loaded, unquarantined shader source kind starts its chain with the
    /// kind's program, which restarts when `shown`, the version of the frame its source
    /// shows, changes.
    pub(crate) fn of(
        source: &Source,
        input: GpuKey,
        shown: u64,
        assets: &FilterAssets,
    ) -> Option<Self> {
        let chain = Self::of_filters(
            &ChainOwner::Source(source.id().clone()),
            source.filters().chain(FilterChain::Video),
            input.clone(),
            assets,
        );
        let SourceKind::Builtin { kind } = source.kind() else {
            return chain;
        };
        let Some(program) = assets.sources().get(kind) else {
            return chain;
        };
        let settings = source.settings();
        let slots = program
            .scene_params()
            .iter()
            .map(|id| {
                match Reference::from_value(settings.get(id)?)? {
                    Reference::Scene { scene_id } => Some(scene_id),
                    _ => None,
                }
                .map(|scene| GpuKey::reference(&scene))
            })
            .collect();
        let first = ChainFilter {
            id: FilterId::new(PROGRAM_FILTER),
            kind: kind.clone(),
            settings: settings.clone(),
            reference: None,
            scene: None,
            effect: None,
            program: Some(Program {
                source: program.clone(),
                slots,
                shown,
                heard: assets
                    .heard()
                    .get(source.id())
                    .map(|scope| Heard(Arc::clone(scope))),
            }),
        };
        let mut chain = chain.unwrap_or(Self {
            input,
            filters: Vec::new(),
            pad: None,
        });
        chain.filters.insert(0, first);
        Some(chain)
    }

    /// The chain of `owner`'s video `filters` over `input`, as [`Self::of`] makes a
    /// source's: a scene's, over its composite.
    pub(crate) fn of_filters(
        owner: &ChainOwner,
        filters: &[Filter],
        input: GpuKey,
        assets: &FilterAssets,
    ) -> Option<Self> {
        let filters: Vec<ChainFilter> = filters
            .iter()
            .filter(|filter| filter.enabled())
            // A follow camera drives its scene's camera and draws nothing (ADR-0120).
            .filter(|filter| filter.kind() != jstream_protocol::FOLLOW_POINTER_KIND)
            // A 3D rendering switch that is off leaves its filter out (ADR-0170 decision 1),
            // so it passes its input through and nothing it reads is drawn for it.
            .filter(|filter| assets.features().draws_filter(filter.kind()))
            .filter_map(|filter| {
                let effect = assets.effects().get(filter.kind()).cloned();
                let drawn = effect.is_some()
                    || VideoFilter::new(filter.kind(), filter.settings()).is_some();
                if !drawn {
                    tracing::debug!(
                        owner = %owner,
                        filter = %filter.id(),
                        kind = filter.kind(),
                        "video filter kind is not drawn by this engine; bypassed"
                    );
                    return None;
                }
                Some(ChainFilter {
                    id: filter.id().clone(),
                    kind: filter.kind().to_owned(),
                    settings: filter.settings().clone(),
                    reference: filter_assets::wanted(filter.kind(), filter.settings())
                        .and_then(|key| assets.get(&key).cloned()),
                    scene: scene_read(filter.kind(), filter.settings())
                        .map(|id| GpuKey::reference(&id)),
                    effect,
                    program: None,
                })
            })
            .collect();
        (!filters.is_empty()).then_some(Self {
            input,
            filters,
            pad: None,
        })
    }

    /// `chain`, a scene's chain over `input`, started with the screen-space reflection
    /// pass (ADR-0170 decision 4) at `steps`, so it runs before every filter of the
    /// scene's own: the chain of a scene with none.
    pub(crate) fn with_screen_reflections(chain: Option<Self>, input: GpuKey, steps: u32) -> Self {
        let mut settings = Settings::default();
        settings.0.insert("steps".into(), serde_json::json!(steps));
        let first = ChainFilter {
            id: FilterId::new(SCREEN_REFLECTIONS_FILTER),
            kind: jstream_render::SCREEN_REFLECTIONS_KIND.to_owned(),
            settings,
            reference: None,
            scene: None,
            effect: None,
            program: None,
        };
        let mut chain = chain.unwrap_or(Self {
            input,
            filters: Vec::new(),
            pad: None,
        });
        chain.filters.insert(0, first);
        chain
    }

    /// Whether one of its filters reads its scene's surface buffer (ADR-0170 decision
    /// 4), so a 3D scene's composite draws one with its view depth.
    pub(crate) fn reads_surface(&self) -> bool {
        self.filters.iter().any(|filter| {
            filter.effect.is_none()
                && filter.program.is_none()
                && jstream_render::video_filter_reads_surface(&filter.kind)
        })
    }

    /// Whether one of its filters reads its scene's view depth (ADR-0143 decision 5), so
    /// a 3D scene's composite draws one for it (decision 4).
    pub(crate) fn reads_depth(&self) -> bool {
        self.filters.iter().any(|filter| {
            filter.effect.is_none()
                && filter.program.is_none()
                && jstream_render::video_filter_reads_depth(&filter.kind)
        })
    }

    /// Whether one of its filters also reads its scene's shadow maps (ADR-0151 decision
    /// 6), so the scene's composite draws them for it.
    pub(crate) fn reads_shadows(&self) -> bool {
        self.filters.iter().any(|filter| {
            filter.effect.is_none()
                && filter.program.is_none()
                && jstream_render::video_filter_reads_shadows(&filter.kind)
        })
    }

    /// The floor the first of its filters that mirrors its scene mirrors it in (ADR-0151
    /// decision 4), so the scene's composite draws the scene mirrored there for it.
    pub(crate) fn reflection_floor(&self) -> Option<f32> {
        self.filters.iter().find_map(|filter| {
            (filter.effect.is_none() && filter.program.is_none())
                .then(|| {
                    jstream_render::video_filter_reflection_floor(&filter.kind, &filter.settings)
                })
                .flatten()
        })
    }

    /// The room its filters need beyond each edge of the input, in canvas pixels: the
    /// sum of what each draws past its input at its settings (ADR-0136), each filter
    /// taking the largest of its own settings' outset and that of each of `also`'s
    /// settings at its place: the look a Move blends from (ADR-0127), or the least and
    /// the most the settings that motion drives reach (ADR-0138), so the layer holds
    /// what the filters draw at any point between. Only an item's effects layer is
    /// padded by it; a source's or a scene's chain draws at its input's size
    /// (ADR-0065 §5).
    pub(crate) fn outset_also(&self, also: &[&[Settings]]) -> u32 {
        self.outsets(also).into_iter().fold(0, u32::saturating_add)
    }

    /// Each filter's outset, as [`Self::outset_also`] takes it.
    fn outsets(&self, also: &[&[Settings]]) -> Vec<u32> {
        self.filters
            .iter()
            .enumerate()
            .map(|(at, filter)| {
                let own = outset_of(&filter.kind, &filter.settings);
                also.iter()
                    .filter_map(|also| also.get(at))
                    .fold(own, |most, other| most.max(outset_of(&filter.kind, other)))
            })
            .collect()
    }

    /// Where an item's effects layer grows, for a layer of `scale` pixels per canvas
    /// pixel (ADR-0065 §5): at the first filter that draws past its input, by the
    /// outset of it and every filter after it, with the outsets of
    /// [`Self::outset_also`]. The filters before it see the item's box itself, so
    /// rounded corners or a vignette frame the box, not the room a later drop shadow
    /// needs. `None` when no filter draws past its input.
    pub(crate) fn pad_at_also(&self, scale: f32, also: &[&[Settings]]) -> Option<(usize, u32)> {
        let outsets = self.outsets(also);
        let first = outsets.iter().position(|px| *px > 0)?;
        let outset = outsets[first..]
            .iter()
            .fold(0u32, |sum, px| sum.saturating_add(*px));
        Some((first, (outset as f32 * scale).ceil() as u32))
    }

    /// The padding filter `index` draws with: [`Self::pad`]'s when it names it, else 0.
    fn pad_of(&self, index: usize) -> u32 {
        match self.pad {
            Some((at, px)) if at == index => px,
            _ => 0,
        }
    }

    /// The kinds of its filters an effect pack draws: its effects, and the shader
    /// source whose program starts it.
    pub(crate) fn pack_kinds(&self) -> impl Iterator<Item = &str> {
        self.filters
            .iter()
            .filter(|filter| filter.effect.is_some() || filter.program.is_some())
            .map(|filter| filter.kind.as_str())
    }

    /// Each filter's `int` parameters, in order, which a Move keeps whole as it blends
    /// their settings (ADR-0127): from a pack effect's descriptor, or the built-in
    /// kind's.
    pub(crate) fn whole_params(&self) -> Vec<Vec<String>> {
        static BUILTIN: OnceLock<BTreeMap<String, Vec<String>>> = OnceLock::new();
        let builtin = BUILTIN.get_or_init(|| {
            jstream_render::video_filter_descriptors()
                .iter()
                .chain(&jstream_render::depth_filter_descriptors())
                .map(|kind| (kind.kind_id.clone(), whole_params(kind)))
                .collect()
        });
        self.filters
            .iter()
            .map(|filter| match &filter.effect {
                Some(effect) => whole_params(effect.kind_descriptor()),
                None => builtin.get(&filter.kind).cloned().unwrap_or_default(),
            })
            .collect()
    }

    /// Each filter's settings as planned, in order.
    pub(crate) fn own_settings(&self) -> Vec<Settings> {
        self.filters.iter().map(|f| f.settings.clone()).collect()
    }

    /// What filter `index`'s parameter `param` is, when a motion modifier can drive it
    /// (ADR-0138): a number, with its smallest and largest value (unbounded ends
    /// infinite) and whether it is whole (`int`), or a colour; with its default, which
    /// a filter saved before its kind gained the parameter draws. `None` for another
    /// type, or a parameter or filter it doesn't have.
    pub(crate) fn drivable(
        &self,
        index: usize,
        param: &str,
    ) -> Option<(crate::driven::Target, serde_json::Value)> {
        use crate::driven::Target;
        static BUILTIN: OnceLock<BTreeMap<String, jstream_protocol::KindDescriptor>> =
            OnceLock::new();
        let builtin = BUILTIN.get_or_init(|| {
            jstream_render::video_filter_descriptors()
                .into_iter()
                .map(|kind| (kind.kind_id.clone(), kind))
                .collect()
        });
        let filter = self.filters.get(index)?;
        let descriptor = match &filter.effect {
            Some(effect) => effect.kind_descriptor(),
            None => builtin.get(&filter.kind)?,
        };
        let described = descriptor.params.iter().find(|p| p.id == param)?;
        let target = match described.ty {
            jstream_protocol::ParamType::Float { min, max, .. } => Some(Target::Number {
                range: (
                    min.unwrap_or(f64::NEG_INFINITY),
                    max.unwrap_or(f64::INFINITY),
                ),
                whole: false,
            }),
            jstream_protocol::ParamType::Int { min, max, .. } => Some(Target::Number {
                range: (
                    min.map_or(f64::NEG_INFINITY, |v| v as f64),
                    max.map_or(f64::INFINITY, |v| v as f64),
                ),
                whole: true,
            }),
            jstream_protocol::ParamType::Color { .. } => Some(Target::Color),
            _ => None,
        }?;
        Some((target, described.default.clone()))
    }

    /// The enabled filters' kinds and settings, in order: what makes two items' effects
    /// look the same, so a transition may pair them (ADR-0065 §6, as ADR-0081 amends it).
    pub(crate) fn look(&self) -> Vec<(String, Settings)> {
        self.filters
            .iter()
            .map(|filter| (filter.kind.clone(), filter.settings.clone()))
            .collect()
    }

    /// Every image it reads as texels: its input and what its filters read
    /// (`scene_mask`, a pack effect's slots). A layer among them is drawn at the canvas
    /// size, as filters work in its pixels (ADR-0249).
    pub(crate) fn reads(&self) -> impl Iterator<Item = &GpuKey> {
        std::iter::once(&self.input).chain(self.filters.iter().flat_map(|filter| {
            let slots = filter.program.iter().flat_map(|p| p.slots.iter().flatten());
            filter.scene.iter().chain(slots)
        }))
    }

    /// The scenes its filters read, in filter order: what must be drawn before it runs.
    /// Each is read from its layer, or from its own chain's output when it has one.
    pub(crate) fn scenes(&self) -> impl Iterator<Item = &SceneId> {
        self.filters
            .iter()
            .flat_map(|filter| {
                let slots = filter.program.iter().flat_map(|p| p.slots.iter().flatten());
                filter.scene.iter().chain(slots)
            })
            .filter_map(|key| match key {
                GpuKey::Layer(scene, _) | GpuKey::Filtered(ChainOwner::Scene(scene)) => Some(scene),
                _ => None,
            })
    }

    /// The scene a `scene` source's chain runs over (ADR-0065 §4): the layer it draws
    /// into for the chain, or its own chain's output. `None` for a chain over a frame,
    /// a capture, a composite or an item's box.
    pub(crate) fn input_scene(&self) -> Option<&SceneId> {
        match &self.input {
            GpuKey::Layer(scene, _) | GpuKey::Filtered(ChainOwner::Scene(scene)) => Some(scene),
            _ => None,
        }
    }

    /// Points each filter that reads a scene `filtered` says has a chain at that chain's
    /// output, so a `scene_mask` sees the scene with its own filters, as it shows
    /// wherever it is placed.
    pub(crate) fn read_filtered(&mut self, filtered: impl Fn(&SceneId) -> bool) {
        for filter in &mut self.filters {
            let slots = filter.program.iter_mut().flat_map(|p| p.slots.iter_mut());
            for key in std::iter::once(&mut filter.scene).chain(slots) {
                if let Some(GpuKey::Layer(scene, _)) = key
                    && filtered(scene)
                {
                    *key = Some(GpuKey::Filtered(ChainOwner::Scene(scene.clone())));
                }
            }
        }
    }

    /// The size of the layer each filter draws into, first filter first, for an input
    /// of `width` by `height`: what [`Chains::run_one`] makes, each filter's output size
    /// carried through the ones before it.
    pub(crate) fn layer_sizes(&self, width: u32, height: u32) -> Vec<(u32, u32)> {
        let mut size = (width, height);
        self.filters
            .iter()
            .enumerate()
            .map(|(index, filter)| {
                if let Some(mut drawn) = filter.video_filter() {
                    drawn.set_pad(self.pad_of(index));
                    size = drawn.output_size(size.0, size.1);
                }
                size
            })
            .collect()
    }

    /// The scratch layer the padded filter copies its input into (ADR-0125), for an
    /// input of `width` by `height`: that filter's input grown by the padding on every
    /// side. `None` for a chain with no padding.
    pub(crate) fn pad_layer(&self, width: u32, height: u32) -> Option<(u32, u32)> {
        let (at, pad) = self.pad.filter(|(_, pad)| *pad > 0)?;
        let input = match at.checked_sub(1) {
            Some(before) => *self.layer_sizes(width, height).get(before)?,
            None => (width, height),
        };
        let grow = pad.saturating_mul(2);
        Some((input.0.saturating_add(grow), input.1.saturating_add(grow)))
    }

    /// The size the chain draws for an input of `width` by `height`: the source's
    /// reported size once filtered (only aspect ratio changes it; a drop shadow, an
    /// outline or a glow draws within the frame, ADR-0065 §5).
    pub(crate) fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        self.filters
            .iter()
            .filter_map(ChainFilter::video_filter)
            .fold((width, height), |(w, h), filter| filter.output_size(w, h))
    }
}

/// A filter as one compositor runs it.
struct Running {
    id: FilterId,
    kind: String,
    settings: Settings,
    /// The pack effect drawing it, for a pack kind: a reload makes the filter again.
    effect: Option<PackEffect>,
    /// The shader source drawing it, for a program, and the frame version it last
    /// restarted at: a reload makes the filter again, a new version restarts it.
    program: Option<(PackSource, u64)>,
    /// The scene layers a program's slots hold, as handed to `filter`, to tell when a
    /// layer is remade.
    slots: Vec<Option<Arc<Image>>>,
    /// What measures a program's audio: made with the filter, or when it starts to hear
    /// some, so on a chain or settings change and never while a steady frame draws.
    meter: Option<Box<AudioMeter>>,
    filter: VideoFilter,
    /// The version of the asset whose image `filter` reads, if any.
    reference: Option<u64>,
    /// The scene layer `filter` reads, if any: the same image the filter holds, kept to
    /// tell when the layer is remade.
    scene: Option<Arc<Image>>,
    /// The layer it draws into ([`output_key`]), made when the chain's shape changes so
    /// a steady frame builds no key.
    key: GpuKey,
    /// Whether a failure to make its layer or draw has been logged, so a filter that
    /// keeps failing logs once; a frame it draws clears it, so a later failure logs
    /// again.
    warned: bool,
}

/// The cache frames that last drew a scene's [`crate::snapshot::GpuKey::Sky`] layer and
/// its [`crate::snapshot::GpuKey::SkyLeaving`] layer.
pub(crate) type SkyDrawn = (Option<u64>, Option<u64>);

/// A scene family's sky backdrop from frame to frame (ADR-0167, ADR-0238).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SkyState {
    /// The cache frame its travel last advanced in, and the media time then.
    pub(crate) stepped: Option<(u64, f64)>,
    /// The warp it streams at now, moving toward its setting at its ramp's pace.
    pub(crate) warp: f32,
    /// How far its stars have moved away from the direction of travel
    /// ([`jstream_render::SkyDraw::travel`]).
    pub(crate) travel: f64,
    /// Whether its warp is heading down toward a lower setting.
    pub(crate) dropping: f32,
    /// With its warp aimed ahead (ADR-0187), the rotation that lays its sky out from
    /// its item's: the sky is drawn turned by its item's rotation after this one. `None`
    /// lays it out as its item turns it.
    pub(crate) aim: Option<crate::warp_aim::Rotation>,
    /// The sky it cross-fades from since its warp last started aimed ahead, while it
    /// does.
    pub(crate) leaving: Option<LeavingSky>,
    /// The arrival picture it shows (ADR-0188), if any.
    pub(crate) arrival: Option<Arrival>,
    /// How many arrival pictures it has picked, which seeds the next pick.
    pub(crate) arrivals: u32,
    /// Which picture it showed last, so the next pick is another.
    pub(crate) last_arrival: Option<u32>,
}

/// The arrival picture a backdrop shows (ADR-0188): which of its scene's pictures, and
/// where it is pinned in the sky's layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Arrival {
    /// Which picture, bottom first; read modulo how many there are now.
    pub(crate) index: u32,
    /// Its centre's direction in the sky's layout (x right, y down, z toward the viewer),
    /// unit length: near straight ahead.
    pub(crate) direction: [f64; 3],
    /// The rotation from the layout it was pinned in to the sky's layout now, once a
    /// warp aimed ahead has laid the sky out anew while it showed (ADR-0224), so it stays
    /// where it is seen; `None` while it is in the layout it was pinned in.
    pub(crate) relaid: Option<crate::warp_aim::Rotation>,
}

impl Arrival {
    /// Where in the sky's layout now its picture `offset_deg` from its first is
    /// ([`crate::snapshot::part_direction`]), turned with the layout it was pinned in.
    pub(crate) fn part(&self, offset_deg: [f32; 2]) -> [f64; 3] {
        let d = crate::snapshot::part_direction(self.direction, offset_deg);
        self.relaid.map_or(d, |r| {
            [0, 1, 2].map(|i| (0..3).map(|k| r[i][k] * d[k]).sum::<f64>())
        })
    }
}

/// How far from the sky's straight ahead an arrival picture is pinned, at most, in
/// degrees across and up (ADR-0188): off centre, but in view as a warp ends.
const ARRIVAL_SPREAD_DEG: [f64; 2] = [9.0, 6.0];

/// The warp at and above which the deep sky, and an arrival picture, is gone (ADR-0167
/// decision 5, `sky.wgsl`'s `deep`).
const DEEP_GONE_AT: f32 = 0.25;

/// A hash of `a` and `b`, spread over 32 bits.
fn hash(a: u32, b: u32) -> u32 {
    let mut x = a.wrapping_mul(0x9e37_79b9) ^ b.wrapping_add(0x7f4a_7c15).rotate_left(13);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85eb_ca6b);
    x ^= x >> 13;
    x = x.wrapping_mul(0xc2b2_ae35);
    x ^ (x >> 16)
}

/// How much the deep sky, and an arrival picture, shows at `warp` while dropping by
/// `dropping`, 0 to 1, as `sky.wgsl` fades it.
pub(crate) fn deep_shown(warp: f32, dropping: f32) -> f32 {
    let smooth = |a: f32, b: f32, x: f32| {
        let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let rising = 1.0 - smooth(0.0, DEEP_GONE_AT, warp);
    let falling = 1.0 - smooth(0.15, 0.55, warp);
    let dropping = dropping.clamp(0.0, 1.0);
    rising + (falling - rising) * dropping
}

/// The warp at and above which an arrival picture is gone going into warp (ADR-0239):
/// its first touch, as the stars start to move, well before the deep sky goes.
const PICTURE_GONE_AT: f32 = 0.05;

/// How much an arrival picture shows at `warp` while dropping by `dropping`, 0 to 1
/// (ADR-0239): gone by [`PICTURE_GONE_AT`] going into warp, back as the deep sky comes
/// back dropping out. Never more than [`deep_shown`].
pub(crate) fn picture_shown(warp: f32, dropping: f32) -> f32 {
    let smooth = |a: f32, b: f32, x: f32| {
        let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let rising = 1.0 - smooth(0.0, PICTURE_GONE_AT, warp);
    let falling = 1.0 - smooth(0.15, 0.55, warp);
    let dropping = dropping.clamp(0.0, 1.0);
    rising + (falling - rising) * dropping
}

/// An arrival picture's own opacity, over the deep sky's fade the sky shader puts on it
/// (`sky.wgsl`'s `deep`), so it shows at [`picture_shown`] in all.
pub(crate) fn picture_opacity(warp: f32, dropping: f32) -> f32 {
    let deep = deep_shown(warp, dropping);
    if deep <= 0.0 {
        0.0
    } else {
        (picture_shown(warp, dropping) / deep).clamp(0.0, 1.0)
    }
}

/// The sky a backdrop cross-fades from as a warp aimed ahead starts (ADR-0187): as it
/// was drawn the frame before, still.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LeavingSky {
    /// Its turn, tilt and rotation, in degrees, as [`jstream_render::Environment`]
    /// takes them.
    pub(crate) angles: [f32; 3],
    /// How far its stars had travelled.
    pub(crate) travel: f64,
    /// The media time the cross-fade began, in seconds.
    pub(crate) since: f64,
    /// The arrival picture it showed (ADR-0188).
    pub(crate) arrival: Option<Arrival>,
}

/// How long a backdrop takes to cross-fade to the sky a warp aimed ahead lays out, in
/// seconds of media time (ADR-0187).
pub(crate) const AIM_FADE_S: f64 = 0.5;

/// How a backdrop's warp is aimed this frame (ADR-0187): its item's turn, tilt and
/// rotation, and with the warp aimed ahead, how far its travel may turn, in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WarpAiming {
    /// Its item's turn, tilt and rotation, in degrees.
    pub(crate) angles: [f32; 3],
    /// With the warp aimed ahead, how far its travel may turn from straight ahead.
    pub(crate) ahead: Option<f32>,
}

impl SkyState {
    /// Advances the sky to `at_s` seconds, once per cache `frame`: its warp moves toward
    /// `target` by one over `ramp_s` seconds (at once for a ramp of 0), and its travel by
    /// [`jstream_render::SkyDraw::travel_rate`] of that warp. The first step starts at
    /// `target`; a step back in time or of over a second, as a seek makes, takes the
    /// warp to `target` and moves nothing, and a step of no time, a held or paused clock
    /// still drawing, keeps it as it is. It is dropping while its warp is still above
    /// `target`, easing out as it nears it.
    ///
    /// With its warp aimed ahead (ADR-0187), a warp that starts from still, or a first
    /// step or a jump that lands at warp, lays the sky out around the scene's straight
    /// ahead as the item turns it now; one that starts from still after a frame drew
    /// cross-fades from that frame's sky over [`AIM_FADE_S`]. At warp, a turn of the
    /// item that takes the travel past its limit is held there, and the slack is let
    /// go, so turning back moves it at once and the warp ends where it is.
    pub(crate) fn step(
        &mut self,
        frame: u64,
        at_s: f64,
        (target, ramp_s): (f32, f32),
        aiming: WarpAiming,
        (pictures, seed): (usize, u32),
    ) {
        let still = self.warp <= 0.0;
        let was = self.warp;
        let first = self.stepped.is_none();
        let shown = self.angles(aiming);
        match self.stepped {
            Some((stepped, _)) if stepped == frame => return,
            // A step of no time (a held or paused clock still drawing) moves nothing and
            // keeps the warp, rather than jumping it to the setting.
            Some((_, last)) if at_s >= last && at_s - last <= 1.0 => {
                let dt = (at_s - last) as f32;
                let before = self.warp;
                self.warp = if ramp_s > 0.0 {
                    let most = dt / ramp_s;
                    before + (target - before).clamp(-most, most)
                } else {
                    target
                };
                // The mean of the two speeds over the step.
                let rate = (jstream_render::SkyDraw::travel_rate(before)
                    + jstream_render::SkyDraw::travel_rate(self.warp))
                    * 0.5;
                self.travel += rate * f64::from(dt);
            }
            _ => self.warp = target,
        }
        // Dropping in full while above the setting, easing out over the last 0.15 toward
        // any setting, 0 included (ADR-0186), so a setting that moves, as a modifier
        // makes it, moves this with it rather than snapping it.
        self.dropping = if target >= self.warp {
            0.0
        } else {
            ((self.warp - target) / 0.15).min(1.0)
        };
        self.aim_at(at_s, aiming, (still, first), shown);
        // Falling, as a setting that follows a channel or a sequence down out of warp
        // makes it, though never above its setting (ADR-0224).
        let falling = !first && self.warp < was;
        self.arrive(pictures, seed, falling);
        self.stepped = Some((frame, at_s));
    }

    /// Stops showing the arrival picture, remembering which it was.
    fn drop_arrival(&mut self) {
        if let Some(arrival) = self.arrival.take() {
            self.last_arrival = Some(arrival.index);
        }
    }

    /// Picks the arrival picture after a step (ADR-0188): of `pictures`, none while the
    /// deep sky is gone ([`deep_shown`]), and a new one at random by `seed` once none is
    /// picked, the deep sky shows and the warp is still, dropping or `falling` this step
    /// (ADR-0224), pinned near straight ahead.
    fn arrive(&mut self, pictures: usize, seed: u32, falling: bool) {
        if pictures == 0 {
            self.drop_arrival();
            return;
        }
        // Tied to what the backdrop shows: none once the deep sky is gone, and one
        // picked once, on the first step it shows again, so a drop out of warp picks
        // exactly one.
        let shown = deep_shown(self.warp, self.dropping);
        if shown <= 0.0 {
            self.drop_arrival();
            return;
        }
        if self.arrival.is_some() || (self.warp > 0.0 && self.dropping <= 0.0 && !falling) {
            return;
        }
        let count = u32::try_from(pictures).unwrap_or(u32::MAX);
        let before = self.last_arrival;
        self.arrivals = self.arrivals.wrapping_add(1);
        let roll = hash(seed, self.arrivals);
        let mut index = roll % count;
        // Not the one it just left, when there is another.
        if count > 1 && before.is_some_and(|before| before % count == index) {
            index = (index + 1) % count;
        }
        let spread = |bits: u32, most: f64| {
            (f64::from(bits & 0xffff) / 65535.0 * 2.0 - 1.0) * most.to_radians()
        };
        let across = spread(roll >> 8, ARRIVAL_SPREAD_DEG[0]);
        let up = spread(hash(roll, seed), ARRIVAL_SPREAD_DEG[1]);
        self.arrival = Some(Arrival {
            index,
            direction: [across.sin() * up.cos(), -up.sin(), -across.cos() * up.cos()],
            relaid: None,
        });
    }

    /// Lays the sky out for the warp's aim after a step ([`Self::step`]): `still`
    /// whether the warp was still before it, `first` whether it was the first, and
    /// `shown` the sky's angles as the frame before would have drawn them.
    fn aim_at(
        &mut self,
        at_s: f64,
        aiming: WarpAiming,
        (still, first): (bool, bool),
        shown: [f32; 3],
    ) {
        use crate::warp_aim::{from_angles, held_ahead, inverse, multiply};
        let Some(limit) = aiming.ahead else {
            self.aim = None;
            self.leaving = None;
            return;
        };
        let item = from_angles(aiming.angles);
        let jumped = self
            .stepped
            .is_some_and(|(_, last)| at_s < last || at_s - last > 1.0);
        if self.warp > 0.0 && (still || jumped) {
            // The warp starts: lay the sky out around straight ahead, the item's
            // rotation undone.
            self.aim = Some(inverse(&item));
            self.leaving = (!first && !jumped).then_some(LeavingSky {
                angles: shown,
                travel: self.travel,
                since: at_s,
                arrival: self.arrival,
            });
            // The picture it showed stays where it is seen, turned into the sky laid out
            // anew, so it fades as the warp rises as the deep sky does (ADR-0224); the
            // sky it cross-fades from shows it there too.
            let now = from_angles(self.angles(aiming));
            if let Some(arrival) = &mut self.arrival {
                let turned = multiply(&inverse(&now), &from_angles(shown));
                arrival.relaid = Some(arrival.relaid.map_or(turned, |r| multiply(&turned, &r)));
            }
        } else if let (true, Some(aim)) = (self.warp > 0.0, self.aim) {
            // At warp: hold the travel within its limit, letting the slack go.
            let laid = multiply(&item, &aim);
            let held = held_ahead(&laid, limit);
            if held != laid {
                self.aim = Some(multiply(&inverse(&item), &held));
            }
        }
        if self
            .leaving
            .is_some_and(|leaving| at_s < leaving.since || at_s - leaving.since >= AIM_FADE_S)
        {
            self.leaving = None;
        }
    }

    /// The turn, tilt and rotation the sky is drawn at, in degrees, for its item's
    /// `aiming`: the item's own, or with its warp aimed ahead (ADR-0187), the item's
    /// after the sky's layout, the travel held within its limit while at warp.
    pub(crate) fn angles(&self, aiming: WarpAiming) -> [f32; 3] {
        use crate::warp_aim::{from_angles, held_ahead, multiply, to_angles};
        let (Some(limit), Some(aim)) = (aiming.ahead, self.aim) else {
            return aiming.angles;
        };
        let laid = multiply(&from_angles(aiming.angles), &aim);
        to_angles(&if self.warp > 0.0 {
            held_ahead(&laid, limit)
        } else {
            laid
        })
    }

    /// How far the sky has cross-faded to the one its warp aimed ahead lays out, 0 to 1,
    /// at `at_s` seconds; 1 when it isn't fading.
    pub(crate) fn faded_in(&self, at_s: f64) -> f32 {
        self.leaving.map_or(1.0, |leaving| {
            ((at_s - leaving.since) / AIM_FADE_S).clamp(0.0, 1.0) as f32
        })
    }
}

/// The filters one compositor runs, by owner, kept from frame to frame.
#[derive(Default)]
pub(crate) struct Chains {
    running: BTreeMap<ChainOwner, Vec<Running>>,
    /// The follow cameras the scenes it draws glide through (ADR-0113 §3): state the
    /// frames carry, like a stateful filter's.
    cameras: std::cell::RefCell<crate::follow::Cameras>,
    /// The owners whose chain failed to draw this frame, which draw nothing.
    failed: BTreeSet<ChainOwner>,
    /// The model items whose surface was drawn with their layer since the chains last
    /// began a frame (ADR-0170 decision 4): the surface buffer merges theirs.
    pub(crate) surfaced: BTreeSet<(jstream_protocol::SceneId, jstream_protocol::ItemId)>,
    /// The items whose effects layer failed to be made or drawn and has been logged, so
    /// one that keeps failing logs once; a frame it draws clears it.
    box_warned: BTreeSet<ChainOwner>,
    /// Filters no shot runs any more, on their way to the cache.
    retired: Vec<VideoFilter>,
    /// The frame of the cache the chains last ran in. Every mix a tick draws shares one
    /// frame, and the chains run once in it, so a stateful filter sees each tick once.
    ran: Option<u64>,
    /// The output stage's images drawn in the frame `ran` names: each mix's
    /// [`GpuKey::Mixed`] once its master chain ran, and each output's chain output, so
    /// a mix two outputs draw at different sizes is composited and filtered once.
    staged: BTreeSet<GpuKey>,
    /// The scene layers the chains' passes drew in the frame `ran` names, which a mix
    /// that nests the same scene at the same depth reads instead of drawing it again
    /// (#1070).
    drawn: BTreeSet<GpuKey>,
    /// The nested scenes placed as their one picture this frame (ADR-0249) whose
    /// picture they show mirrored, across and down: an item that places one mirrors
    /// it so.
    pub(crate) mirrored: BTreeMap<GpuKey, (bool, bool)>,
    /// The 3D models this compositor draws (ADR-0107), uploaded once per version, by
    /// the source that shows them.
    models: BTreeMap<SourceId, (u64, jstream_render::GpuModel)>,
    /// The models drawn since the chains last began a frame: one no frame drew is let
    /// go when the next begins.
    models_used: BTreeSet<SourceId>,
    /// How many pixels a 3D model's layer is drawn with, as a percentage of its
    /// canvas's width and of its height (ADR-0197): 100 unless the program stage's
    /// render thread lowered it to keep its frames in time.
    pub(crate) model_percent: crate::resolution::Percent,
    /// Whether this frame's 3D extras are drawn cheaper while the GPU is short of time
    /// ([`crate::resolution::Governor::economy`]): its models with one sample a pixel,
    /// and its ambient occlusion and volumetric fog at their lowest quality
    /// (`ViewDepth::economy`).
    pub(crate) economy: bool,
    /// Whether a model's layer was drawn since this was last cleared: the render thread
    /// lowers [`Self::model_percent`] only while there is one to gain from it.
    pub(crate) model_drawn: bool,
    /// The thread that uploads the models, for the program stage's render thread
    /// ([`Self::upload_models_aside`]). `None` uploads them where they are drawn.
    models_aside: Option<crate::model_uploads::ModelUploads>,
    /// The lights of the scene a model is drawn in this frame (ADR-0143 decision 3),
    /// kept to reuse its allocation.
    pub(crate) lights: Vec<jstream_render::SceneLight>,
    /// The scenes whose lights past the limit have been logged, so each logs once.
    pub(crate) lights_warned: BTreeSet<jstream_protocol::SceneId>,
    /// The scenes whose extra environment lights were logged, once each (ADR-0151
    /// decision 9).
    pub(crate) environments_warned: BTreeSet<jstream_protocol::SceneId>,
    /// Each 3D scene family's sky backdrop (ADR-0167): how far its stars have travelled
    /// at warp, and the arrival it shows. One per family, so every view of a scene
    /// shows the same sky (ADR-0238).
    pub(crate) skies: std::cell::RefCell<BTreeMap<jstream_protocol::SceneId, SkyState>>,
    /// Each drawn scene's sky layers: the cache frame that last drew its
    /// [`GpuKey::Sky`] layer and the one that last drew its
    /// [`GpuKey::SkyLeaving`] layer, as a scene places each only in the frame that drew it.
    pub(crate) skies_drawn: std::cell::RefCell<BTreeMap<jstream_protocol::SceneId, SkyDrawn>>,
    /// The average colour of each arrival picture a backdrop shows now (ADR-0188),
    /// straight sRGB, with the version of the frame it was taken from: the colour its
    /// light shines in. A picture no longer shown is let go.
    pub(crate) picture_means: BTreeMap<SourceId, (u64, [f32; 3])>,
    /// Each 3D scene's view depth (ADR-0143 decision 4), as its composite last drew it,
    /// with the frame of the chains' run that drew it: a scene's depth-reading filters
    /// read it only in that run. Kept, like a scene layer a filter reads, to tell when
    /// the layer is remade.
    depths: BTreeMap<jstream_protocol::SceneId, (u64, jstream_render::ViewDepth)>,
    /// Each 3D scene's shadow maps (ADR-0151 decision 3) with the frame of the chains'
    /// run that drew them: drawn once a run, by the first `scene`-lit model there that
    /// receives shadows.
    shadows: BTreeMap<jstream_protocol::SceneId, (u64, Vec<crate::snapshot::DrawnShadow>)>,
    /// What each scene's shadow maps were last drawn of, to leave a map that still
    /// holds its shadows as it is.
    pub(crate) shadow_maps: BTreeMap<jstream_protocol::SceneId, crate::snapshot::KeptShadows>,
    /// How many shadow maps were drawn, for tests.
    #[cfg(test)]
    pub(crate) shadow_draws: std::cell::Cell<u64>,
    /// How many times a model was drawn through its scene's own camera, into its layer,
    /// its view depth or its surface, for tests.
    #[cfg(test)]
    pub(crate) scene_model_draws: std::cell::Cell<u64>,
    /// The shadowed lights of the scene a model is drawn in this frame, by index into
    /// [`Self::lights`] with their softness, kept to reuse its allocation.
    pub(crate) shadowed: Vec<(usize, f32)>,
    /// The scenes whose shadowed lights past the limit have been logged.
    pub(crate) shadows_warned: BTreeSet<jstream_protocol::SceneId>,
    /// Each reflection probe this compositor has captured (ADR-0170 decision 3), by
    /// scene and item: what it was captured with and when, to tell when it is due.
    pub(crate) probes: BTreeMap<
        (jstream_protocol::SceneId, jstream_protocol::ItemId),
        crate::snapshot::ProbeState,
    >,
    /// The frame each reflection probe last tried to capture in, whether or not it drew,
    /// so the one capture a frame goes to the probe that has waited longest.
    pub(crate) probe_tries: BTreeMap<(jstream_protocol::SceneId, jstream_protocol::ItemId), u64>,
    /// The reflection probe capture under way, which draws one face a frame.
    pub(crate) probe_run: Option<crate::snapshot::ProbeRun>,
    /// How many frames the chains ran in, for tests.
    #[cfg(test)]
    runs: usize,
    /// How many mix passes read a layer [`Self::drawn`] holds, for tests.
    #[cfg(test)]
    pub(crate) reused: std::sync::atomic::AtomicUsize,
}

impl Chains {
    /// Starts a frame of the chains in `chains`, between the caller's
    /// [`ResourceCache::begin_frame`] and [`ResourceCache::end_frame`]: keeps a
    /// [`VideoFilter`] for each of their filters, retiring the ones no chain runs any
    /// more. Returns `false` when they already ran in the cache's current frame, so the
    /// caller runs none of them again: every mix a tick draws shares one frame, and a
    /// stateful filter sees each tick once. Each chain then runs with [`Self::run_one`].
    /// `stages` are the output stage's chains (ADR-0080), kept the same way, which
    /// [`Self::stage`] runs once each per frame. `blended` holds, for a chain whose item
    /// a Move blends between two looks (ADR-0127), the settings each of its filters
    /// draws with this frame, in place of the plan's.
    pub(crate) fn begin(
        &mut self,
        compositor: &mut Compositor,
        cache: &mut ResourceCache<GpuKey>,
        chains: &BTreeMap<ChainOwner, Chain>,
        stages: &BTreeMap<ChainOwner, Chain>,
        blended: &[(&ChainOwner, Vec<Settings>)],
    ) -> bool {
        let frame = cache.frame();
        if self.ran == Some(frame) {
            return false;
        }
        // A view depth the last run didn't draw is let go with the cache's images.
        let last = self.ran;
        self.depths.retain(|_, (drawn, depth)| {
            let keep = Some(*drawn) == last;
            if !keep {
                cache.retire_image(Image::clone(&depth.image));
            }
            keep
        });
        // So are the shadow maps kept for a scene whose shadows the last run didn't
        // draw: a scene that is gone, or shown no more.
        let shadows = &self.shadows;
        self.shadow_maps.retain(|scene, _| {
            shadows
                .get(scene)
                .is_some_and(|(drawn, _)| Some(*drawn) == last)
        });
        self.ran = Some(frame);
        #[cfg(test)]
        {
            self.runs += 1;
        }
        self.failed.clear();
        self.surfaced.clear();
        self.staged.clear();
        self.drawn.clear();
        self.mirrored.clear();
        let used = std::mem::take(&mut self.models_used);
        self.models.retain(|source, _| used.contains(source));
        if let Some(aside) = &mut self.models_aside {
            // The uploads that came back, for the models the last frame still drew.
            let models = &mut self.models;
            aside.finished(
                |source| used.contains(source),
                |source, version, model| {
                    models.insert(source, (version, model));
                },
            );
        }
        let retired = &mut self.retired;
        self.running.retain(|source, running| {
            let keep = chains.contains_key(source) || stages.contains_key(source);
            if !keep {
                retired.extend(running.drain(..).map(|r| r.filter));
            }
            keep
        });
        for (source, chain) in chains.iter().chain(stages) {
            if !self.running.contains_key(source) {
                self.running.insert(source.clone(), Vec::new());
            }
            if let Some(running) = self.running.get_mut(source) {
                let blend = blended
                    .iter()
                    .find(|(owner, _)| *owner == source)
                    .map(|(_, settings)| settings.as_slice());
                sync(source, running, &chain.filters, blend, &mut self.retired);
                for (r, f) in running.iter_mut().zip(&chain.filters) {
                    refer(compositor, source, r, f, &mut self.retired);
                }
            }
        }
        for filter in self.retired.drain(..) {
            cache.retire_filter(filter);
        }
        true
    }

    /// Runs `source`'s `chain` at `at`, after [`Self::begin`] started the frame, once its
    /// input is in `cache` and the layer of every scene it reads has been drawn this
    /// frame ([`GpuKey::reference`]). A chain whose input is not cached (a failed or
    /// skipped upload), or whose scene layer hasn't been drawn yet (only a chain closing
    /// a cycle, which the graph refuses), draws nothing. A failed scene pass fails the
    /// frame before this runs.
    pub(crate) fn run_one(
        &mut self,
        compositor: &mut Compositor,
        cache: &mut ResourceCache<GpuKey>,
        (source, chain): (&ChainOwner, &Chain),
        (at, beat): (MediaTime, f64),
    ) {
        let Some(running) = self.running.get_mut(source) else {
            return;
        };
        let Some(input) = cache.get(&chain.input) else {
            self.failed.insert(source.clone());
            return;
        };
        // Every output layer first, so the draws below read and write the cache
        // without borrowing it mutably. Each is placed at what its input is placed at,
        // carried through the filters' sizes (ADR-0073), as the input's own size is.
        let mut size = (input.width(), input.height());
        let mut placed = input.extent();
        for (index, filter) in running.iter_mut().enumerate() {
            let pad = chain.pad_of(index);
            filter.filter.set_pad(pad);
            let input = size;
            size = filter.filter.output_size(size.0, size.1);
            placed = placed_after(
                filter.filter.output_size(placed.0, placed.1),
                placed,
                (input, size),
                pad,
            );
            let made = cache
                .layer(compositor, &filter.key, size.0, size.1)
                .map(|_| ());
            if let Ok(extent) = FrameSize::new(placed.0, placed.1) {
                cache.set_extent(&filter.key, extent);
            }
            if let Err(error) = made {
                if !filter.warned {
                    filter.warned = true;
                    tracing::warn!(
                        source = %source,
                        filter = %filter.id,
                        kind = %filter.kind,
                        %error,
                        "video filter output can't be made; its owner draws nothing"
                    );
                }
                self.failed.insert(source.clone());
                return;
            }
        }
        for (r, f) in running.iter_mut().zip(&chain.filters) {
            if let Some(program) = &f.program
                && !fill(cache, source, r, &program.slots)
            {
                self.failed.insert(source.clone());
                return;
            }
            if let Some(Program {
                heard: Some(heard), ..
            }) = &f.program
            {
                hear(r, heard, at);
            }
            // A depth-aware filter reads its scene's view depth drawn this run, and
            // anywhere else has none, so it passes its input through (ADR-0143).
            if r.filter.reads_depth() {
                let depth = match source {
                    ChainOwner::Scene(scene) => self
                        .depths
                        .get(scene)
                        .filter(|(drawn, _)| Some(*drawn) == self.ran)
                        .map(|(_, depth)| depth.clone()),
                    _ => None,
                };
                r.filter.set_depth(depth);
            }
            let Some(scene) = &f.scene else {
                continue;
            };
            if !see(cache, source, r, scene) {
                self.failed.insert(source.clone());
                return;
            }
        }
        let clock = FilterClock {
            seconds: at.as_secs_f64(),
            beat,
        };
        let cache = &*cache;
        let mut input = &chain.input;
        for (index, running) in running.iter_mut().enumerate() {
            let Running {
                id,
                kind,
                filter,
                key,
                warned,
                ..
            } = running;
            // Each filter's GPU time is its own part of its owner's (`profile.get`). A
            // model source's filters are timed with its item's effects, whose places
            // they would share, so they are no part of it.
            let part = (!matches!(source, ChainOwner::Shown(..)))
                .then_some(crate::profile::GpuPart::Filter(index));
            crate::profile::gpu_part(compositor, part);
            let drawn = match (cache.get(input), cache.get(key)) {
                (Some(from), Some(to)) => compositor.apply_filter_into(filter, from, to, clock),
                _ => Err(RenderError::Invalid {
                    what: "cache",
                    detail: format!("no image for {input:?} or {key:?}"),
                }),
            };
            crate::profile::gpu_part(compositor, None);
            if let Err(error) = drawn {
                if !*warned {
                    *warned = true;
                    tracing::warn!(
                        source = %source,
                        filter = %id,
                        kind = %kind,
                        %error,
                        "video filter failed to draw; its owner draws nothing"
                    );
                }
                self.failed.insert(source.clone());
                return;
            }
            *warned = false;
            input = key;
        }
    }

    /// Records that the output stage's `key` is drawn in this frame, and returns whether
    /// it was not yet: the caller draws it only then (ADR-0080 §1, once per tick for
    /// each mix).
    pub(crate) fn stage(&mut self, key: &GpuKey) -> bool {
        if self.staged.contains(key) {
            return false;
        }
        self.staged.insert(key.clone());
        true
    }

    /// Marks the layers the chains drew into last time as used in the cache's current
    /// frame, without drawing, unless they already ran in it: a mix not due on a tick
    /// keeps what its chains drew, and their layers aren't retired and reallocated.
    pub(crate) fn claim(&self, compositor: &Compositor, cache: &mut ResourceCache<GpuKey>) {
        if self.ran == Some(cache.frame()) {
            return;
        }
        for running in self.running.values().flatten() {
            let key = &running.key;
            let Some(size) = cache.get(key).map(|image| (image.width(), image.height())) else {
                continue;
            };
            // It is the same size, so this reuses it and can't fail; if it ever does, the
            // next run makes it again.
            let _ = cache.layer(compositor, key, size.0, size.1);
        }
    }

    /// Records that scene `scene`'s view depth was drawn into `key` in this run, seen
    /// through `view` and lit by `lights`, for its chain's depth-aware filters to read
    /// (ADR-0143 decision 5, ADR-0151 decision 1), with this run's shadow maps of the
    /// scene when `shadows` (decision 6). The lights and maps are copied only when they
    /// change. A layer remade since the last frame (the first, or one at
    /// another size) retires the one it replaces into `cache` (R-RT-003); a steady frame
    /// makes no layer and no new handle to it.
    pub(crate) fn drew_depth(
        &mut self,
        cache: &mut ResourceCache<GpuKey>,
        scene: &jstream_protocol::SceneId,
        key: &GpuKey,
        (view, lights): (jstream_protocol::CameraView, &[jstream_render::SceneLight]),
        shadows: bool,
        (reflection, surface): DrawnBeside<'_>,
    ) {
        let reflection = reflection.and_then(|(image, depth, floor_y)| {
            Some(jstream_render::DepthReflection {
                image: cache.get(image)?.clone(),
                depth: cache.get(depth)?.clone(),
                floor_y,
            })
        });
        let surface = surface.and_then(|[normal, brdf, reflected]| {
            Some(jstream_render::DepthSurface {
                normal: cache.get(normal)?.clone(),
                brdf: cache.get(brdf)?.clone(),
                reflected: cache.get(reflected)?.clone(),
            })
        });
        let (Some(ran), Some(layer)) = (self.ran, cache.get(key)) else {
            return;
        };
        let focal_px = view.focal_px();
        // This run's maps, as `(light, key, camera)`, and whether `held` already holds
        // them.
        let maps = shadows
            .then(|| self.shadows.get(scene))
            .flatten()
            .filter(|(drawn, _)| *drawn == ran)
            .map_or(&[][..], |(_, maps)| maps.as_slice());
        let same = |held: &[jstream_render::DepthShadow]| {
            held.len() == maps.len()
                && held.iter().zip(maps).all(|(held, map)| {
                    held.light == map.light
                        && held.camera == map.camera
                        && cache
                            .get(&map.key)
                            .is_some_and(|image| image.texture() == held.image.texture())
                })
        };
        let owned = |cache: &ResourceCache<GpuKey>| -> Arc<[jstream_render::DepthShadow]> {
            maps.iter()
                .filter_map(|map| {
                    Some(jstream_render::DepthShadow {
                        light: map.light,
                        image: cache.get(&map.key)?.clone(),
                        camera: map.camera,
                    })
                })
                .collect()
        };
        if let Some((drawn, depth)) = self.depths.get_mut(scene)
            && depth.image.texture() == layer.texture()
        {
            *drawn = ran;
            depth.focal_px = focal_px;
            depth.view = Some(view);
            if *depth.lights != *lights {
                depth.lights = Arc::from(lights);
            }
            if !same(&depth.shadows) {
                depth.shadows = owned(cache);
            }
            depth.reflection = reflection;
            depth.surface = surface;
            depth.economy = self.economy;
            return;
        }
        let fresh = jstream_render::ViewDepth {
            view: Some(view),
            economy: self.economy,
            lights: Arc::from(lights),
            shadows: owned(cache),
            reflection,
            surface,
            ..jstream_render::ViewDepth::new(Arc::new(layer.clone()), focal_px)
        };
        if let Some((_, old)) = self.depths.insert(scene.clone(), (ran, fresh)) {
            cache.retire_image(Image::clone(&old.image));
        }
    }

    /// Scene `scene`'s shadow maps when this run has drawn them, else `None`.
    pub(crate) fn shadows_of(
        &self,
        scene: &jstream_protocol::SceneId,
    ) -> Option<&[crate::snapshot::DrawnShadow]> {
        let ran = self.ran?;
        self.shadows
            .get(scene)
            .filter(|(drawn, _)| *drawn == ran)
            .map(|(_, maps)| maps.as_slice())
    }

    /// Records that this run drew scene `scene`'s shadow maps: `maps`, maybe none.
    pub(crate) fn drew_shadows(
        &mut self,
        scene: &jstream_protocol::SceneId,
        maps: Vec<crate::snapshot::DrawnShadow>,
    ) {
        if let Some(ran) = self.ran {
            self.shadows.insert(scene.clone(), (ran, maps));
        }
    }

    /// Marks `owner`'s chain as failed this frame, so what shows its output draws
    /// nothing: an item whose box couldn't be drawn.
    pub(crate) fn fail(&mut self, owner: &ChainOwner) {
        self.failed.insert(owner.clone());
    }

    /// Marks the effects layer of `owner` as failed this frame, as [`Self::fail`] does,
    /// and logs `error` once until a frame draws it: a failed layer blanks only its
    /// item, as a failed chain blanks only its owner.
    pub(crate) fn fail_box(&mut self, owner: &ChainOwner, error: &RenderError) {
        self.fail(owner);
        if self.box_warned.insert(owner.clone()) {
            tracing::warn!(
                owner = %owner,
                %error,
                "item effects layer can't be made or drawn; the item draws nothing"
            );
        }
    }

    /// Records that `owner`'s effects layer drew, so a later failure logs again.
    /// Records that the chains' passes drew scene layer `key` this frame.
    pub(crate) fn drew_layer(&mut self, key: &GpuKey) {
        self.drawn.insert(key.clone());
    }

    /// Whether the chains' passes drew scene layer `key` this frame, so a mix's pass
    /// for it reads that layer as it is instead of drawing it again.
    pub(crate) fn reuse(&self, key: &GpuKey) -> bool {
        let drawn = self.holds_layer(key);
        #[cfg(test)]
        if drawn {
            self.reused
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        drawn
    }

    /// [`Self::reuse`] without counting a read: whether the chains' passes drew scene
    /// layer `key` this frame.
    pub(crate) fn holds_layer(&self, key: &GpuKey) -> bool {
        self.drawn.contains(key)
    }

    pub(crate) fn drew_box(&mut self, owner: &ChainOwner) {
        self.box_warned.remove(owner);
    }

    /// The follow cameras of the scenes this context draws (ADR-0113 §3).
    pub(crate) fn cameras(&self) -> std::cell::RefMut<'_, crate::follow::Cameras> {
        self.cameras.borrow_mut()
    }

    /// Whether `key`, an image a layer shows, is a chain output that failed this frame.
    pub(crate) fn failed(&self, key: &GpuKey) -> bool {
        match key {
            // A model's source's filters draw nothing over a layer that failed.
            GpuKey::Filtered(owner @ ChainOwner::Shown(_, scene, item)) => {
                self.failed.contains(owner)
                    || self
                        .failed
                        .contains(&ChainOwner::Item(scene.clone(), item.clone()))
            }
            GpuKey::Filtered(owner) => self.failed.contains(owner),
            // A model's layer, shown as drawn when it has no effects (ADR-0107).
            GpuKey::ItemBox(scene, item) => self
                .failed
                .contains(&ChainOwner::Item(scene.clone(), item.clone())),
            _ => false,
        }
    }

    /// Has the models `compositor` draws uploaded by a thread of their own from now on
    /// (ADR-0177): for the program stage's render thread, which must not stop for an
    /// upload.
    pub(crate) fn upload_models_aside(&mut self, compositor: &Compositor) {
        let uploader = compositor.model_uploader();
        self.models_aside = Some(crate::model_uploads::ModelUploads::new(move |model| {
            uploader.upload(model)
        }));
    }

    /// Whether a model's upload was asked of the thread and isn't back, for tests.
    #[cfg(test)]
    pub(crate) fn models_coming(&self) -> bool {
        self.models_aside
            .as_ref()
            .is_some_and(crate::model_uploads::ModelUploads::busy)
    }

    /// Uploads the model `source` shows, unless the upload kept for it is of the same
    /// version (ADR-0107): drawing it again then allocates nothing. With a thread for
    /// the uploads ([`Self::upload_models_aside`]) it only asks for the upload, which a
    /// later frame's [`Self::begin`] takes: [`Self::model`] is the version before
    /// until then, or none.
    ///
    /// # Errors
    /// What the compositor reports for the upload.
    pub(crate) fn upload_model(
        &mut self,
        compositor: &Compositor,
        source: &SourceId,
        model: &std::sync::Arc<jstream_media::model::Model>,
    ) -> Result<(), RenderError> {
        self.models_used.insert(source.clone());
        if self
            .models
            .get(source)
            .is_some_and(|(version, _)| *version == model.version())
        {
            return Ok(());
        }
        if let Some(aside) = &mut self.models_aside {
            match aside.ask(source, model) {
                Ok(crate::model_uploads::Asked::Coming) => return Ok(()),
                Ok(crate::model_uploads::Asked::Here) => {}
                Err(error) => {
                    // A refused version is not drawn as the one before it.
                    self.models.remove(source);
                    return Err(error);
                }
            }
        }
        // The old upload goes first, so two versions are never held at once.
        self.models.remove(source);
        let uploaded = compositor.upload_model(model)?;
        self.models
            .insert(source.clone(), (model.version(), uploaded));
        Ok(())
    }

    /// Keeps `models`, which the warm scenes' sources show, uploaded though no frame
    /// draws them (ADR-0177 decision 5): each is uploaded as a drawn one is
    /// ([`Self::upload_model`]) and not let go of when the next frame begins. One whose
    /// upload is refused is left for the draw that shows it to report.
    pub(crate) fn keep_models(
        &mut self,
        compositor: &Compositor,
        models: &BTreeMap<SourceId, std::sync::Arc<jstream_media::model::Model>>,
    ) {
        for (source, model) in models {
            let _refused = self.upload_model(compositor, source, model);
        }
    }

    /// The upload kept for the model `source` shows.
    pub(crate) fn model(&self, source: &SourceId) -> Option<&jstream_render::GpuModel> {
        self.models.get(source).map(|(_, model)| model)
    }

    /// How many filters run, for tests.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.running.values().map(Vec::len).sum()
    }
}

/// Where the filter at index `at` of `source`'s chain draws: the last one into the
/// chain's output, the others into their own layer.
fn output_key(source: &ChainOwner, at: usize, last: usize) -> GpuKey {
    if at == last {
        GpuKey::Filtered(source.clone())
    } else {
        GpuKey::Chain(source.clone(), at)
    }
}

/// Makes `running` run `filters`: a filter with the same ID and kind as the one at its
/// place keeps its state and takes new settings in place; any other change rebuilds the
/// list, keeping each filter whose ID and kind it finds, and gives each its layer's key
/// for `source`'s chain. A pack filter whose effect was reloaded is made again with the
/// new effect. Allocates nothing when nothing changed.
fn sync(
    source: &ChainOwner,
    running: &mut Vec<Running>,
    filters: &[ChainFilter],
    blended: Option<&[Settings]>,
    retired: &mut Vec<VideoFilter>,
) {
    // The settings filter `at` draws with: the blend's this frame, or the plan's.
    fn settings<'a>(
        blended: Option<&'a [Settings]>,
        at: usize,
        f: &'a ChainFilter,
    ) -> &'a Settings {
        blended
            .and_then(|blended| blended.get(at))
            .unwrap_or(&f.settings)
    }
    let same_shape = running.len() == filters.len()
        && running
            .iter()
            .zip(filters)
            .all(|(r, f)| r.id == f.id && r.kind == f.kind);
    if same_shape {
        for (at, (r, f)) in running.iter_mut().zip(filters).enumerate() {
            update(r, f, settings(blended, at, f), retired);
        }
        return;
    }
    let mut old: Vec<Running> = std::mem::take(running);
    for (at, f) in filters.iter().enumerate() {
        let kept = old
            .iter()
            .position(|r| r.id == f.id && r.kind == f.kind)
            .map(|at| old.swap_remove(at));
        let entry = match kept {
            Some(mut r) => {
                update(&mut r, f, settings(blended, at, f), retired);
                r
            }
            None => match f.video_filter() {
                Some(mut filter) => Running {
                    id: f.id.clone(),
                    kind: f.kind.clone(),
                    settings: {
                        let settings = settings(blended, at, f);
                        if *settings != f.settings {
                            filter.set_settings(settings);
                        }
                        settings.clone()
                    },
                    effect: f.effect.clone(),
                    program: f.program.as_ref().map(|p| (p.source.clone(), p.shown)),
                    slots: Vec::new(),
                    meter: meter_for(f),
                    filter,
                    reference: None,
                    scene: None,
                    key: GpuKey::Filtered(source.clone()),
                    warned: false,
                },
                // The plan only keeps kinds this engine draws.
                None => continue,
            },
        };
        running.push(entry);
    }
    let last = running.len().saturating_sub(1);
    for (at, r) in running.iter_mut().enumerate() {
        r.key = output_key(source, at, last);
    }
    retired.extend(old.into_iter().map(|r| r.filter));
}

/// Gives `running` `settings` (`filter`'s, or a Move's blend of them, ADR-0127) and the
/// rest of `filter`, the same filter as the plan has it now: in
/// place, or, when its pack effect was reloaded, by making it again with the new effect
/// and retiring the old one. A pack effect keeps no state between frames, so nothing is
/// lost.
///
/// A program is made again when its shader source was reloaded, losing its feedback,
/// and restarted when its source showed a new frame.
fn update(
    running: &mut Running,
    filter: &ChainFilter,
    settings: &Settings,
    retired: &mut Vec<VideoFilter>,
) {
    let program = filter.program.as_ref().map(|p| (p.source.clone(), p.shown));
    let reloaded = running.program.as_ref().map(|(source, _)| source)
        != program.as_ref().map(|(source, _)| source);
    if running.effect != filter.effect || reloaded {
        if let Some(mut fresh) = filter.video_filter() {
            if *settings != filter.settings {
                fresh.set_settings(settings);
            }
            retired.push(std::mem::replace(&mut running.filter, fresh));
        }
        running.effect = filter.effect.clone();
        running.program = program;
        // The new filter holds no slot yet.
        running.slots.clear();
        running.settings = settings.clone();
        if running.meter.is_none() {
            running.meter = meter_for(filter);
        }
        return;
    }
    if let (Some((_, shown)), Some((_, now))) = (&mut running.program, &program)
        && shown != now
    {
        running.filter.restart();
        *shown = *now;
    }
    if running.settings != *settings {
        running.filter.set_settings(settings);
        running.settings = settings.clone();
    }
    if running.meter.is_none() {
        running.meter = meter_for(filter);
    }
}

/// A meter for `filter` when it is a program that hears a scope: its FFT is planned and
/// its buffers made here, so measuring allocates nothing.
fn meter_for(filter: &ChainFilter) -> Option<Box<AudioMeter>> {
    filter
        .program
        .as_ref()
        .filter(|p| p.heard.is_some())
        .map(|_| Box::new(AudioMeter::new()))
}

/// Gives `running` the image of the asset `filter` reads, when that asset changed: the
/// compositor makes the image once per load. A kind that reads a reference keeps no
/// state between frames, so it is made again with the new image, and the old one, which
/// holds the old image, is retired with the cache's images (R-RT-003). An image that
/// can't be made is logged, and the filter passes its input through until the asset
/// changes.
fn refer(
    compositor: &mut Compositor,
    source: &ChainOwner,
    running: &mut Running,
    filter: &ChainFilter,
    retired: &mut Vec<VideoFilter>,
) {
    let wanted = filter.reference.as_ref().map(|asset| asset.version);
    if running.reference == wanted {
        return;
    }
    running.reference = wanted;
    let Some(mut fresh) = filter.video_filter() else {
        return;
    };
    if let Some(asset) = &filter.reference {
        match filter_assets::reference_image(compositor, asset) {
            Ok(image) => fresh.set_reference(Some(Arc::new(image))),
            Err(error) => tracing::warn!(
                source = %source,
                filter = %filter.id,
                kind = %filter.kind,
                %error,
                "video filter's asset can't be put on the GPU; the filter passes its input through"
            ),
        }
    }
    retired.push(std::mem::replace(&mut running.filter, fresh));
}

/// The scene a filter of `kind` with `settings` reads: `scene_mask`'s `scene`, or `None`
/// for another kind or a `null` setting.
fn scene_read(kind: &str, settings: &Settings) -> Option<SceneId> {
    if kind != "scene_mask" {
        return None;
    }
    match Reference::from_value(settings.0.get("scene")?)? {
        Reference::Scene { scene_id } => Some(scene_id),
        _ => None,
    }
}

/// Measures what `running`, a program, hears from `heard` at `at`, and hands it over for
/// this frame. The meter was made with the filter or when it started to hear
/// ([`meter_for`]), so this allocates nothing and takes no lock.
fn hear(running: &mut Running, heard: &Heard, at: MediaTime) {
    let Some(meter) = running.meter.as_mut() else {
        return;
    };
    let ms = at
        .rescale(
            jstream_media::Timebase::MILLIS,
            jstream_media::Rounding::Down,
        )
        .map_or(0, |t| u64::try_from(t.ticks()).unwrap_or(0));
    let metered = meter.measure(&heard.0, ms);
    running.filter.set_audio(ProgramAudio {
        level: metered.level,
        peak: metered.peak,
        onset: metered.onset,
        bands: metered.bands,
        wave: metered.wave,
    });
}

/// Hands `running`, a program, the scene layers `slots` name as drawn this frame: each
/// one remade since (the first, or one at another size) goes into its slot in place, so
/// the program keeps its feedback, and the one it replaces is retired into `cache`
/// (R-RT-003). An empty slot reads transparent. Returns `false`, logged once, when a
/// layer isn't there.
fn fill(
    cache: &mut ResourceCache<GpuKey>,
    source: &ChainOwner,
    running: &mut Running,
    slots: &[Option<GpuKey>],
) -> bool {
    running.slots.resize(slots.len(), None);
    for (slot, key) in slots.iter().enumerate() {
        let layer = match key {
            Some(key) => {
                let Some(layer) = cache.get(key) else {
                    if !running.warned {
                        running.warned = true;
                        tracing::warn!(
                            source = %source,
                            kind = %running.kind,
                            ?key,
                            "a scene a shader source reads wasn't drawn; it draws nothing"
                        );
                    }
                    return false;
                };
                Some(layer)
            }
            None => None,
        };
        let held = &running.slots[slot];
        let same = match (held, layer) {
            (Some(held), Some(layer)) => held.texture() == layer.texture(),
            (None, None) => true,
            _ => false,
        };
        if same {
            continue;
        }
        let fresh = layer.map(|layer| Arc::new(layer.clone()));
        running.filter.set_input(slot, fresh.clone());
        if let Some(old) = std::mem::replace(&mut running.slots[slot], fresh) {
            cache.retire_image(Image::clone(&old));
        }
    }
    true
}

/// Hands `running`, a filter that reads the scene layer `key`, that layer as drawn this
/// frame. A new layer (the first, or one remade at another size) makes the filter again
/// with it and retires the old one, with the layer it held, into `cache`, so nothing
/// the filter held is dropped on the render thread (R-RT-003). The scene mask keeps no
/// state between frames, so nothing is lost. Returns `false`, logged once, when the
/// layer isn't there.
fn see(
    cache: &mut ResourceCache<GpuKey>,
    source: &ChainOwner,
    running: &mut Running,
    key: &GpuKey,
) -> bool {
    let Some(layer) = cache.get(key) else {
        if !running.warned {
            running.warned = true;
            tracing::warn!(
                source = %source,
                filter = %running.id,
                kind = %running.kind,
                ?key,
                "the scene a video filter reads wasn't drawn; its owner draws nothing"
            );
        }
        return false;
    };
    if running
        .scene
        .as_ref()
        .is_some_and(|held| held.texture() == layer.texture())
    {
        return true;
    }
    let Some(mut fresh) = make(&running.kind, &running.settings, running.effect.as_ref()) else {
        return true;
    };
    let held = Arc::new(layer.clone());
    fresh.set_reference(Some(Arc::clone(&held)));
    running.scene = Some(held);
    cache.retire_filter(std::mem::replace(&mut running.filter, fresh));
    true
}

/// The extent of a filter's output, from `grown`, its input's extent `placed` grown as
/// its texels are: the pad counts in texels (ADR-0219), so over an input with fewer
/// texels than its extent (an item's layer scaled down to fit, #1720) it grows the
/// extent by as many of its own pixels as it stands for, keeping the texels per extent
/// pixel the filters after it scale by. A filter that sizes its output otherwise keeps
/// `grown`.
fn placed_after(
    grown: (u32, u32),
    placed: (u32, u32),
    (input, output): ((u32, u32), (u32, u32)),
    pad: u32,
) -> (u32, u32) {
    let twice = pad.saturating_mul(2);
    let padded = (input.0.saturating_add(twice), input.1.saturating_add(twice));
    if pad == 0 || output != padded || placed == input {
        return grown;
    }
    let side = |placed: u32, texels: u32| {
        let pad = (f64::from(pad) * f64::from(placed) / f64::from(texels.max(1))).round();
        placed.saturating_add((pad as u32).saturating_mul(2))
    };
    (side(placed.0, input.0), side(placed.1, input.1))
}

#[cfg(test)]
mod tests {
    use jstream_graph::Graph;
    use jstream_media::FrameSize;
    use jstream_media::{MediaTime, Timebase};
    use jstream_protocol::{
        Edit, FilterChain, FilterId, FilterOwner, ItemId, OutputId, OutputKind, RecordContainer,
        SceneId, Settings, SourceId, SourceKind, VideoMixId,
    };
    use std::sync::Arc;

    use jstream_render::{Compositor, Lut, ResourceCache};
    use jstream_sources::StillImage;

    use super::{ChainOwner, Chains};
    use crate::filter_assets::{self, FilterAsset, FilterAssets, Loaded};
    use crate::snapshot::tests::gpu;
    use crate::snapshot::{self, Frame, GpuKey, LiveParams, Look, NoCaptures, Shot};

    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];

    /// Scene `main` placing the sources `logo` and then `cam` at the canvas's top left,
    /// with one `color_correction` filter multiplying by red for each `(source,
    /// enabled)` in `filters`.
    fn graph(filters: &[(&str, bool)]) -> Graph {
        let red: Settings = [("multiply", serde_json::json!("#ff0000"))]
            .into_iter()
            .collect();
        let filters: Vec<_> = filters
            .iter()
            .map(|&(source, enabled)| (source, "color_correction", red.clone(), enabled))
            .collect();
        graph_with(&filters)
    }

    /// [`graph`] with one filter of `kind` with `settings` for each `(source, kind,
    /// settings, enabled)` in `filters`.
    fn graph_with(filters: &[(&str, &str, Settings, bool)]) -> Graph {
        let mut edits = vec![Edit::SceneCreate {
            scene_id: SceneId::new("main"),
            name: "Main".into(),
            role: None,
        }];
        for id in ["logo", "cam"] {
            edits.push(Edit::SourceCreate {
                source_id: SourceId::new(id),
                name: id.into(),
                kind: SourceKind::Builtin {
                    kind: "display".into(),
                },
                settings: Default::default(),
                duplicated_from: None,
            });
            edits.push(Edit::ItemAdd {
                scene_id: SceneId::new("main"),
                item_id: ItemId::new(id),
                source_id: SourceId::new(id),
                transform: None,
                geometry: None,
                opacity: None,
                blend: None,
            });
        }
        for (at, (source, kind, settings, enabled)) in filters.iter().enumerate() {
            edits.push(Edit::FilterAdd {
                owner: FilterOwner::Source {
                    source_id: SourceId::new(*source),
                },
                chain: FilterChain::Video,
                filter_id: FilterId::new(format!("f{at}")),
                kind: (*kind).into(),
                settings: settings.clone(),
                enabled: Some(*enabled),
                index: None,
            });
        }
        let mut graph = Graph::new().with_kinds(crate::kinds::builtin());
        graph.apply(&edits, None).expect("edits");
        graph
    }

    /// `graph`'s program shot, every source showing a white 16x9 frame.
    fn shot(graph: &Graph) -> Shot {
        shot_with(graph, &FilterAssets::default())
    }

    /// [`shot`] with the video filters' `assets` loaded.
    fn shot_with(graph: &Graph, assets: &FilterAssets) -> Shot {
        let white: Vec<u8> = std::iter::repeat_n(WHITE, 16 * 9).flatten().collect();
        let frame = Frame::new(16, 9, &white, 1).expect("invariant: 16x9 RGBA8");
        snapshot::plan_with(
            graph,
            Some(&SceneId::new("main")),
            None,
            &mut |_| Some(Look::frame(frame.clone(), None)),
            assets,
        )
        .shot
    }

    /// Only a source with an enabled video filter gets a chain, and a shot with one
    /// changes with time.
    #[test]
    fn a_chain_is_planned_for_each_source_whose_filter_runs() {
        let plain = shot(&graph(&[]));
        assert_eq!(plain.chain_count(), 0);
        assert!(!plain.changes_with_time());

        let filtered = shot(&graph(&[("cam", true), ("logo", false)]));
        assert_eq!(filtered.chain_count(), 1, "the disabled filter is bypassed");
        assert!(filtered.changes_with_time());
        assert_ne!(plain, filtered);
    }

    /// Draws `shot`'s program at the canvas size `mixes` times in one cache frame, as
    /// the render thread draws a tick's mixes, and reads back the top-left pixel.
    fn draw(
        compositor: &mut Compositor,
        (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
        shot: &Shot,
        mixes: usize,
    ) -> [u8; 4] {
        cache.begin_frame();
        let canvas = shot.canvas().expect("canvas");
        let at = MediaTime::zero(Timebase::MILLIS);
        let mut pixel = None;
        for _ in 0..mixes {
            let output = snapshot::compose(
                compositor,
                (&mut *cache, &mut *chains),
                (shot, at),
                snapshot::View::default(),
                &LiveParams::default(),
                &NoCaptures,
                snapshot::DrawAt::read(canvas),
            )
            .expect("compose");
            pixel = output.pixels().expect("read back").pixel(1, 1);
        }
        drop(cache.end_frame(compositor));
        pixel.expect("a pixel")
    }

    /// Draws `shot`'s program once, in a cache frame of its own, with `live` applied,
    /// and reads back the pixels at `points`.
    fn draw_points(
        compositor: &mut Compositor,
        (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
        (shot, live): (&Shot, &LiveParams),
        points: &[(u32, u32)],
    ) -> Vec<[u8; 4]> {
        cache.begin_frame();
        let canvas = shot.canvas().expect("canvas");
        let output = snapshot::compose(
            compositor,
            (&mut *cache, &mut *chains),
            (shot, MediaTime::zero(Timebase::MILLIS)),
            snapshot::View::default(),
            live,
            &NoCaptures,
            snapshot::DrawAt::read(canvas),
        )
        .expect("compose");
        let pixels = output.pixels().expect("read back");
        let read = points
            .iter()
            .map(|&(x, y)| pixels.pixel(x, y).expect("a pixel"))
            .collect();
        drop(cache.end_frame(compositor));
        read
    }

    /// The filter draws over the source's frame before it is placed; two mixes drawn
    /// in one cache frame run the chain once, and a shot without the chain retires its
    /// filter.
    #[test]
    fn a_chain_runs_once_per_frame_and_its_output_is_placed() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let filtered = shot(&graph(&[("cam", true)]));
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &filtered, 2);
        assert_eq!(pixel, RED, "white multiplied by red");
        assert_eq!((chains.runs, chains.len()), (1, 1));

        let plain = shot(&graph(&[]));
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &plain, 1);
        assert_eq!(pixel, WHITE);
        assert_eq!((chains.runs, chains.len()), (2, 0), "the filter is retired");
    }

    /// A filter of an effect pack's kind is bypassed until its pack loads, then draws
    /// like a built-in kind; a reloaded effect draws its new shader on the next frame.
    #[test]
    fn a_pack_filter_draws_once_its_pack_loads() {
        use crate::effect_packs::test_pack::{self, Temp};

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut graph = graph(&[]).with_kinds([test_pack::descriptor()]);
        apply(
            &mut graph,
            vec![Edit::FilterAdd {
                owner: FilterOwner::Source {
                    source_id: SourceId::new("cam"),
                },
                chain: FilterChain::Video,
                filter_id: FilterId::new("tint"),
                kind: test_pack::KIND.into(),
                settings: Settings::default(),
                enabled: None,
                index: None,
            }],
        );
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();

        let unloaded = shot_with(&graph, &FilterAssets::default());
        assert_eq!(unloaded.chain_count(), 0, "bypassed before its pack loads");
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &unloaded, 1);
        assert_eq!(pixel, WHITE);

        let temp = Temp::new("chain");
        let effects = |effect| {
            FilterAssets::new().with_effects(Arc::new(
                [(test_pack::KIND.to_owned(), effect)].into_iter().collect(),
            ))
        };
        let loaded = shot_with(&graph, &effects(test_pack::effect(&temp, test_pack::MUL)));
        assert_eq!(loaded.chain_count(), 1);
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &loaded, 1);
        assert_eq!(pixel, [0, 255, 0, 255], "white times the default green");

        let reloaded = shot_with(&graph, &effects(test_pack::effect(&temp, test_pack::BLUE)));
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &reloaded, 1);
        assert_eq!(pixel, [0, 0, 255, 255], "the reloaded shader");
        assert_eq!(chains.len(), 1, "the filter was made again, not added");
    }

    /// A 3D rendering switch that is off leaves its filter out of the plan (ADR-0170
    /// decision 1), and on again puts it back; a filter no switch names stays.
    #[test]
    fn a_rendering_switch_that_is_off_leaves_its_filter_out() {
        let graph = graph_with(&[
            ("logo", "ambient_occlusion", Settings::default(), true),
            ("cam", "color_correction", Settings::default(), true),
        ]);
        assert_eq!(shot(&graph).chain_count(), 2, "every switch on");
        let off = jstream_protocol::RenderFeatures {
            ambient_occlusion: false,
            ..jstream_protocol::RenderFeatures::ALL
        };
        let planned = shot_with(&graph, &FilterAssets::new().with_features(off));
        assert_eq!(
            planned.chain_count(),
            1,
            "only the color correction is left"
        );
    }

    /// A shader source (ADR-0093) draws its program as its picture, keeping its
    /// feedback from frame to frame; a new frame from its stand-in (it was shown again)
    /// restarts it, and a scene its `scene` parameter names fills its input.
    #[test]
    fn a_shader_source_draws_its_program_restarts_on_show_and_reads_its_scene() {
        use crate::effect_packs::test_pack::{self, Temp};

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let temp = Temp::new("chain-source");
        let source = test_pack::source(&temp);
        let assets = FilterAssets::new().with_sources(Arc::new(
            [(test_pack::SOURCE_KIND.to_owned(), source)]
                .into_iter()
                .collect(),
        ));
        // `glow` in scene `main`, over `logo` and `cam`; scene `sky` shows `logo`.
        let mut graph = graph(&[]).with_kinds(assets.sources().values().map(|s| s.descriptor()));
        apply(
            &mut graph,
            vec![
                Edit::SourceCreate {
                    source_id: SourceId::new("glow"),
                    name: "Glow".into(),
                    kind: SourceKind::Builtin {
                        kind: test_pack::SOURCE_KIND.into(),
                    },
                    settings: Settings::default(),
                    duplicated_from: None,
                },
                Edit::ItemAdd {
                    scene_id: SceneId::new("main"),
                    item_id: ItemId::new("glow"),
                    source_id: SourceId::new("glow"),
                    transform: None,
                    geometry: None,
                    opacity: None,
                    blend: None,
                },
            ],
        );
        let white: Vec<u8> = std::iter::repeat_n(WHITE, 16 * 9).flatten().collect();
        let plan = |graph: &Graph, version: u64| {
            let frame = Frame::new(16, 9, &white, version).expect("invariant: 16x9 RGBA8");
            snapshot::plan_with(
                graph,
                Some(&SceneId::new("main")),
                None,
                &mut |_| Some(Look::frame(frame.clone(), None)),
                &assets,
            )
            .shot
        };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let shot = plan(&graph, 1);
        assert_eq!(
            shot.pack_kinds(),
            [test_pack::SOURCE_KIND.to_owned()].into()
        );
        let mut grey = || draw(&mut compositor, (&mut cache, &mut chains), &shot, 1)[0];
        let (first, second, third) = (grey(), grey(), grey());
        assert!(first < second && second < third, "{first} {second} {third}");
        // Shown again: its stand-in's new frame restarts it.
        let shown = plan(&graph, 2);
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &shown, 1);
        assert_eq!(pixel[0], first);

        apply(
            &mut graph,
            vec![
                Edit::SceneCreate {
                    scene_id: SceneId::new("sky"),
                    name: "Sky".into(),
                    role: None,
                },
                Edit::ItemAdd {
                    scene_id: SceneId::new("sky"),
                    item_id: ItemId::new("sky-logo"),
                    source_id: SourceId::new("logo"),
                    transform: None,
                    geometry: None,
                    opacity: None,
                    blend: None,
                },
                Edit::SourceSetSettings {
                    source_id: SourceId::new("glow"),
                    patch: [(
                        "sky",
                        serde_json::json!({ "type": "scene", "scene_id": "sky" }),
                    )]
                    .into_iter()
                    .collect(),
                },
            ],
        );
        let sky = plan(&graph, 2);
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &sky, 1);
        assert_eq!(pixel, WHITE, "the sky scene shows where it has alpha");
    }

    /// A shader source whose pack isn't loaded (or whose kind is quarantined, which
    /// leaves it out of the loaded sources) runs no program; a new size makes its
    /// feedback layers again, so it starts over from transparent.
    #[test]
    fn a_shader_source_runs_nothing_unloaded_and_starts_over_at_a_new_size() {
        use crate::effect_packs::test_pack::{self, Temp};

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let temp = Temp::new("chain-source-size");
        let source = test_pack::source(&temp);
        let loaded = FilterAssets::new().with_sources(Arc::new(
            [(test_pack::SOURCE_KIND.to_owned(), source.clone())]
                .into_iter()
                .collect(),
        ));
        let mut graph = graph(&[]).with_kinds([source.descriptor()]);
        let glow = |settings: Settings| Edit::SourceCreate {
            source_id: SourceId::new("glow"),
            name: "Glow".into(),
            kind: SourceKind::Builtin {
                kind: test_pack::SOURCE_KIND.into(),
            },
            settings,
            duplicated_from: None,
        };
        apply(
            &mut graph,
            vec![
                glow(Settings::default()),
                Edit::ItemAdd {
                    scene_id: SceneId::new("main"),
                    item_id: ItemId::new("glow"),
                    source_id: SourceId::new("glow"),
                    transform: None,
                    geometry: None,
                    opacity: None,
                    blend: None,
                },
            ],
        );
        let unloaded = shot_with(&graph, &FilterAssets::default());
        assert!(
            unloaded.pack_kinds().is_empty(),
            "no program while unloaded"
        );
        assert_eq!(unloaded.chain_count(), 0);

        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let shot = shot_with(&graph, &loaded);
        let mut grey = |shot: &Shot| draw(&mut compositor, (&mut cache, &mut chains), shot, 1)[0];
        let (first, second) = (grey(&shot), grey(&shot));
        assert!(first < second, "{first} {second}");
        apply(
            &mut graph,
            vec![Edit::SourceSetSettings {
                source_id: SourceId::new("glow"),
                patch: [("width", serde_json::json!(32))].into_iter().collect(),
            }],
        );
        let wider = shot_with(&graph, &loaded);
        assert_eq!(grey(&wider), first, "new layers start transparent");
        assert_eq!(chains.len(), 1, "the program took the size in place");
    }

    /// A shader source that hears audio measures its scope each frame it draws: its
    /// level and spectrum reach the program.
    #[test]
    fn a_shader_source_hears_its_scope() {
        use crate::effect_packs::test_pack::{self, Temp};

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let temp = Temp::new("chain-ear");
        let ear = test_pack::ear(&temp);
        let scope = Arc::new(jstream_sources::AudioScope::new());
        let heard = [(SourceId::new("ear"), Arc::clone(&scope))]
            .into_iter()
            .collect();
        let assets = FilterAssets::new()
            .with_sources(Arc::new(
                [(test_pack::EAR_KIND.to_owned(), ear.clone())]
                    .into_iter()
                    .collect(),
            ))
            .with_heard(Arc::new(heard));
        let mut graph = graph(&[]).with_kinds([ear.descriptor()]);
        apply(
            &mut graph,
            vec![
                Edit::SourceCreate {
                    source_id: SourceId::new("ear"),
                    name: "Ear".into(),
                    kind: SourceKind::Builtin {
                        kind: test_pack::EAR_KIND.into(),
                    },
                    settings: Settings::default(),
                    duplicated_from: None,
                },
                Edit::ItemAdd {
                    scene_id: SceneId::new("main"),
                    item_id: ItemId::new("ear"),
                    source_id: SourceId::new("ear"),
                    transform: None,
                    geometry: None,
                    opacity: None,
                    blend: None,
                },
            ],
        );
        let shot = shot_with(&graph, &assets);
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &shot, 1);
        assert_eq!(pixel, BLACK, "silence");
        // A full-scale 1 kHz sine at 48 kHz, stereo.
        let samples: Vec<f32> = (0..2048)
            .flat_map(|n| {
                let s = (std::f32::consts::TAU * 1000.0 * n as f32 / 48_000.0).sin();
                [s, s]
            })
            .collect();
        scope.push(48_000, &samples);
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &shot, 1);
        assert_eq!(pixel, WHITE, "the tone");
    }

    /// Applies `edits` to `graph`.
    fn apply(graph: &mut Graph, edits: Vec<Edit>) {
        graph.apply(&edits, None).expect("edits");
    }

    /// A `color_correction` filter multiplying by red on scene `scene`'s chain.
    fn scene_red(scene: &str, enabled: bool) -> Edit {
        Edit::FilterAdd {
            owner: FilterOwner::Scene {
                scene_id: SceneId::new(scene),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new("grade"),
            kind: "color_correction".into(),
            settings: [("multiply", serde_json::json!("#ff0000"))]
                .into_iter()
                .collect(),
            enabled: Some(enabled),
            index: None,
        }
    }

    /// `graph`'s shot with `program` on program, every source a white 16x9 frame.
    fn shot_of(graph: &Graph, program: &str) -> Shot {
        let white: Vec<u8> = std::iter::repeat_n(WHITE, 16 * 9).flatten().collect();
        let frame = Frame::new(16, 9, &white, 1).expect("invariant: 16x9 RGBA8");
        snapshot::plan_with(
            graph,
            Some(&SceneId::new(program)),
            None,
            &mut |_| Some(Look::frame(frame.clone(), None)),
            &FilterAssets::default(),
        )
        .shot
    }

    /// A scene's chain (ADR-0065) runs over its composite, once per frame however many
    /// mixes draw it, and the program shows its output; a disabled one is bypassed.
    #[test]
    fn a_scene_chain_runs_over_its_composite_once_per_frame() {
        let mut bypassed = graph(&[]);
        apply(&mut bypassed, vec![scene_red("main", false)]);
        let plain = shot(&bypassed);
        assert!(plain.scene_chain(&SceneId::new("main")).is_none());
        assert_eq!(
            plain,
            shot(&graph(&[])),
            "a disabled filter changes nothing"
        );

        let mut graded = graph(&[]);
        apply(&mut graded, vec![scene_red("main", true)]);
        let filtered = shot(&graded);
        let chain = filtered
            .scene_chain(&SceneId::new("main"))
            .expect("the scene's chain");
        assert_eq!(chain.input, GpuKey::Composite(SceneId::new("main")));
        assert_eq!(
            filtered.chain_passes(),
            [("main".to_owned(), 0)],
            "composited at the program's depth, so it nests as deep as without a filter"
        );
        assert!(filtered.changes_with_time());

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &filtered, 2);
        assert_eq!(pixel, RED, "the white composite multiplied by red");
        assert_eq!((chains.runs, chains.len()), (1, 1));
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &plain, 1);
        assert_eq!(pixel, WHITE);
        assert_eq!(chains.len(), 0, "the filter is retired");
    }

    /// A scene source shows its scene with the scene's filters, and so does a keyframe
    /// scene, which shows its parent's (ADR-0024 §2).
    #[test]
    fn a_nested_or_keyframe_scene_shows_the_filtered_picture() {
        let mut graded = graph(&[]);
        apply(
            &mut graded,
            vec![
                scene_red("main", true),
                Edit::SceneCreate {
                    scene_id: SceneId::new("outer"),
                    name: "Outer".into(),
                    role: None,
                },
                Edit::SourceCreate {
                    source_id: SourceId::new("nest"),
                    name: "Nest".into(),
                    kind: SourceKind::Scene {
                        scene_id: SceneId::new("main"),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
                Edit::ItemAdd {
                    scene_id: SceneId::new("outer"),
                    item_id: ItemId::new("nest"),
                    source_id: SourceId::new("nest"),
                    transform: None,
                    geometry: None,
                    opacity: None,
                    blend: None,
                },
                Edit::SceneCreateVariant {
                    scene_id: SceneId::new("key"),
                    variant_of: SceneId::new("main"),
                    name: "Key".into(),
                },
            ],
        );
        let outer = shot_of(&graded, "outer");
        assert!(outer.scene_chain(&SceneId::new("main")).is_some());
        assert!(outer.scene_chain(&SceneId::new("outer")).is_none());
        assert_eq!(
            outer.chain_passes(),
            [("main".to_owned(), 0)],
            "a filtered nested scene is composited once, at depth 0, wherever it is placed"
        );
        let key = shot_of(&graded, "key");
        assert!(
            key.scene_chain(&SceneId::new("key")).is_some(),
            "the parent's filters"
        );

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        for shot in [outer, key] {
            let mut cache = ResourceCache::new();
            let mut chains = Chains::default();
            let pixel = draw(&mut compositor, (&mut cache, &mut chains), &shot, 1);
            assert_eq!(pixel, RED);
        }
    }

    /// A frame that only claims layers (a mix not due on that tick) keeps the chain's
    /// output layer, so the next frame that draws reuses it instead of reallocating.
    #[test]
    fn a_frame_that_only_claims_layers_keeps_the_chain_output() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let filtered = shot(&graph(&[("cam", true)]));
        let key = GpuKey::Filtered(ChainOwner::Source(SourceId::new("cam")));
        draw(&mut compositor, (&mut cache, &mut chains), &filtered, 1);
        let output = cache
            .get(&key)
            .expect("the chain's output")
            .texture()
            .clone();
        let canvas = filtered.canvas().expect("canvas");
        for _ in 0..3 {
            cache.begin_frame();
            snapshot::claim_layers(
                &mut compositor,
                (&mut cache, &mut chains),
                &filtered,
                snapshot::View::default(),
                canvas,
            )
            .expect("claim");
            drop(cache.end_frame(&compositor));
            assert_eq!(cache.pending(), 0, "nothing retired");
            assert!(cache.get(&key).expect("kept").texture() == &output);
        }
        draw(&mut compositor, (&mut cache, &mut chains), &filtered, 1);
        assert!(cache.get(&key).expect("kept").texture() == &output);
        assert_eq!(chains.runs, 2, "claiming runs nothing");
    }

    /// A 2-point `.cube` table that inverts every colour.
    const INVERT: &str = "LUT_3D_SIZE 2\n1 1 1\n0 1 1\n1 0 1\n0 0 1\n1 1 0\n0 1 0\n1 0 0\n0 0 0\n";

    /// A filter that reads an asset passes its input through until the plan hands the
    /// asset over, then filters with it: a LUT inverts, and a mask cut out by alpha
    /// shows the black under it. A new load of the asset gives the filter a new image.
    #[test]
    fn a_filter_reads_its_asset_once_it_is_loaded() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let asset =
            |path: &str| serde_json::json!({"type": "asset", "root": "collection", "path": path});
        let lut: Settings = [("lut", asset("invert.cube"))].into_iter().collect();
        let mask: Settings = [("image", asset("clear.png"))].into_iter().collect();
        let lut_key = filter_assets::wanted("apply_lut", &lut).expect("a LUT");
        let mask_key = filter_assets::wanted("image_mask", &mask).expect("a mask");
        let table = |version| {
            Arc::new(FilterAsset {
                version,
                loaded: Loaded::Lut(Lut::parse_cube(INVERT).expect("cube")),
            })
        };
        let clear = Arc::new(FilterAsset {
            version: 3,
            loaded: Loaded::Image(StillImage {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 0],
            }),
        });
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let mut assets = FilterAssets::default();
        // `cam` is drawn over `logo` at the top left, so the LUT on `cam` decides the
        // pixel, and the mask has to cut both out for the black under them to show.
        let graded = graph_with(&[("cam", "apply_lut", lut.clone(), true)]);
        let cam_only = |assets: &FilterAssets| shot_with(&graded, assets);
        let pixel = draw(
            &mut compositor,
            (&mut cache, &mut chains),
            &cam_only(&assets),
            1,
        );
        assert_eq!(pixel, WHITE, "no table yet: passed through");
        assets.insert(lut_key.clone(), table(1));
        let pixel = draw(
            &mut compositor,
            (&mut cache, &mut chains),
            &cam_only(&assets),
            1,
        );
        assert_eq!(pixel, [0, 0, 0, 255], "inverted");
        assets.insert(lut_key, table(2));
        let pixel = draw(
            &mut compositor,
            (&mut cache, &mut chains),
            &cam_only(&assets),
            1,
        );
        assert_eq!(pixel, [0, 0, 0, 255], "the new load inverts too");

        let masked = shot_with(
            &graph_with(&[
                ("cam", "image_mask", mask.clone(), true),
                ("logo", "image_mask", mask.clone(), true),
            ]),
            &FilterAssets::from([(mask_key, clear)]),
        );
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &masked, 1);
        assert_eq!(pixel, [0, 0, 0, 255], "cut out: the black under it");
    }

    /// `graph(&[])` with one `aspect_ratio` filter on `cam`, padding to a custom
    /// `width:height`.
    fn padded(width: i64, height: i64) -> Graph {
        let mut graph = graph(&[]);
        let settings = [
            ("ratio", serde_json::json!("custom")),
            ("custom_width", serde_json::json!(width)),
            ("custom_height", serde_json::json!(height)),
            ("mode", serde_json::json!("pad")),
        ];
        graph
            .apply(
                &[Edit::FilterAdd {
                    owner: FilterOwner::Source {
                        source_id: SourceId::new("cam"),
                    },
                    chain: FilterChain::Video,
                    filter_id: FilterId::new("pad"),
                    kind: "aspect_ratio".into(),
                    settings: settings.into_iter().collect(),
                    enabled: Some(true),
                    index: None,
                }],
                None,
            )
            .expect("edit");
        graph
    }

    /// A filter whose output layer can't be made (1:10000 padding of a 16x9 frame is
    /// past any adapter's limit) fails its source every frame but is logged once, and
    /// a frame it draws clears that, so a later failure logs again.
    #[test]
    fn a_filter_whose_layer_cant_be_made_fails_and_logs_once() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let cam = ChainOwner::Source(SourceId::new("cam"));
        let warned = |chains: &Chains| chains.running[&cam][0].warned;
        let failing = shot(&padded(1, 10_000));
        for _ in 0..2 {
            let pixel = draw(&mut compositor, (&mut cache, &mut chains), &failing, 1);
            assert_eq!(pixel, WHITE, "cam draws nothing; logo shows beneath it");
            assert!(chains.failed(&GpuKey::Filtered(cam.clone())));
            assert!(warned(&chains), "logged, and not again");
        }
        let drawing = shot(&padded(16, 9));
        draw(&mut compositor, (&mut cache, &mut chains), &drawing, 1);
        assert!(!chains.failed(&GpuKey::Filtered(cam.clone())));
        assert!(!warned(&chains), "a frame it draws clears the flag");
        assert_eq!(chains.len(), 1, "the settings changed in place");
    }

    /// Scene `main` with `logo` turned red under `cam`, whose `scene_mask` reads scene
    /// `matte`: one source `spot` over the whole canvas, with a `color_correction` at
    /// `spot_opacity` percent. `spot`'s chain sorts after `cam`'s, so `cam` only sees
    /// `spot` if the chains run by what they read.
    fn masked(spot_opacity: f64) -> Graph {
        let red: Settings = [("multiply", serde_json::json!("#ff0000"))]
            .into_iter()
            .collect();
        let mut graph = graph_with(&[("logo", "color_correction", red, true)]);
        let filter = |source: &str, id: &str, kind: &str, settings: Settings| Edit::FilterAdd {
            owner: FilterOwner::Source {
                source_id: SourceId::new(source),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new(id),
            kind: kind.into(),
            settings,
            enabled: Some(true),
            index: None,
        };
        let canvas = jstream_protocol::ItemGeometry::covering_canvas(1920, 1080).expect("canvas");
        let edits = [
            Edit::SceneCreate {
                scene_id: SceneId::new("matte"),
                name: "Matte".into(),
                role: None,
            },
            Edit::SourceCreate {
                source_id: SourceId::new("spot"),
                name: "spot".into(),
                kind: SourceKind::Builtin {
                    kind: "display".into(),
                },
                settings: Default::default(),
                duplicated_from: None,
            },
            Edit::ItemAdd {
                scene_id: SceneId::new("matte"),
                item_id: ItemId::new("spot"),
                source_id: SourceId::new("spot"),
                transform: None,
                geometry: Some(canvas),
                opacity: None,
                blend: None,
            },
            filter(
                "spot",
                "fade",
                "color_correction",
                [("opacity", serde_json::json!(spot_opacity))]
                    .into_iter()
                    .collect(),
            ),
            filter(
                "cam",
                "mask",
                "scene_mask",
                [(
                    "scene",
                    serde_json::json!({"type": "scene", "scene_id": "matte"}),
                )]
                .into_iter()
                .collect(),
            ),
        ];
        graph.apply(&edits, None).expect("edits");
        graph
    }

    /// A `scene_mask` reads its scene as drawn this frame: the scene's pass runs after
    /// the chains of what it places and before the chain that reads it, so the first
    /// frame already shows `cam` through an opaque `spot`, and a transparent one cuts
    /// `cam` out to show the red `logo` beneath. Each frame runs the chains once.
    #[test]
    fn a_scene_mask_reads_its_scene_drawn_first() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let opaque = shot(&masked(100.0));
        assert_eq!(opaque.chain_count(), 3, "cam, logo and spot");
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &opaque, 2);
        assert_eq!(pixel, WHITE, "cam shows where spot is opaque");
        assert_eq!(chains.runs, 1);

        let clear = shot(&masked(0.0));
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &clear, 1);
        assert_eq!(pixel, RED, "cut out where spot is clear: logo shows");
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &opaque, 1);
        assert_eq!(pixel, WHITE, "the same filter reads the scene again");
        assert_eq!(chains.runs, 3);
    }

    /// A `color_correction` effect multiplying by `colour` on item `item`.
    fn item_tint(item: &str, colour: &str) -> Edit {
        Edit::FilterAdd {
            owner: FilterOwner::Item {
                item_id: ItemId::new(item),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new("tint"),
            kind: "color_correction".into(),
            settings: [("multiply", serde_json::json!(colour))]
                .into_iter()
                .collect(),
            enabled: Some(true),
            index: None,
        }
    }

    /// An item's effects (ADR-0065) run over its box, once per frame however many mixes
    /// draw it, into a layer the size of the box; the program shows their output.
    #[test]
    fn an_item_effect_runs_over_its_box_once_per_frame() {
        let mut tinted = graph(&[]);
        apply(&mut tinted, vec![item_tint("cam", "#ff0000")]);
        let filtered = shot(&tinted);
        let (main, cam) = (SceneId::new("main"), ItemId::new("cam"));
        let chain = filtered.item_chain(&main, &cam).expect("cam's effects");
        assert_eq!(chain.input, GpuKey::ItemBox(main.clone(), cam.clone()));
        assert!(filtered.item_chain(&main, &ItemId::new("logo")).is_none());
        assert!(
            filtered.chain(&SourceId::new("cam")).is_none(),
            "not the source's"
        );

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &filtered, 2);
        assert_eq!(pixel, RED, "cam's white box multiplied by red");
        assert_eq!((chains.runs, chains.len()), (1, 1));
        cache.begin_frame();
        let layer = cache
            .get(&GpuKey::ItemBox(main, cam))
            .map(|image| (image.width(), image.height()));
        drop(cache.end_frame(&compositor));
        assert_eq!(layer, Some((16, 9)), "the box of an unscaled 16x9 frame");
    }

    /// A keyframe scene's overridden effects (ADR-0088) run over the item's box where
    /// the keyframe draws it: its parent, and a keyframe that follows the item's own
    /// effects, draw the item without them, and an empty override draws none.
    #[test]
    fn a_keyframe_scene_draws_an_item_with_its_overridden_effects() {
        let (main, key, plain, cam) = (
            SceneId::new("main"),
            SceneId::new("key"),
            SceneId::new("plain"),
            ItemId::new("cam"),
        );
        let red = jstream_protocol::FilterInfo {
            filter_id: FilterId::new("tint"),
            kind: "color_correction".into(),
            enabled: true,
            settings: [("multiply", serde_json::json!("#ff0000"))]
                .into_iter()
                .collect(),
        };
        let mut keyed = graph(&[]);
        let variant = |scene_id: &SceneId| Edit::SceneCreateVariant {
            scene_id: scene_id.clone(),
            variant_of: main.clone(),
            name: scene_id.as_str().into(),
        };
        apply(
            &mut keyed,
            vec![
                variant(&key),
                variant(&plain),
                Edit::ItemSetEffects {
                    scene_id: key.clone(),
                    item_id: cam.clone(),
                    effects: vec![red],
                },
            ],
        );
        let on_key = shot_of(&keyed, "key");
        let chain = on_key
            .item_chain(&key, &cam)
            .expect("the keyframe's effects");
        assert_eq!(chain.input, GpuKey::ItemBox(key.clone(), cam.clone()));
        let on_main = shot_of(&keyed, "main");
        assert!(
            on_main.item_chain(&main, &cam).is_none(),
            "the parent's own"
        );
        let on_plain = shot_of(&keyed, "plain");
        assert!(
            on_plain.item_chain(&plain, &cam).is_none(),
            "follows the item"
        );

        // An empty override draws no effects, over an item that has its own.
        apply(
            &mut keyed,
            vec![
                item_tint("cam", "#ff0000"),
                Edit::ItemSetEffects {
                    scene_id: plain.clone(),
                    item_id: cam.clone(),
                    effects: Vec::new(),
                },
            ],
        );
        assert!(shot_of(&keyed, "main").item_chain(&main, &cam).is_some());
        let none = shot_of(&keyed, "plain");
        assert!(none.item_chain(&plain, &cam).is_none(), "an empty override");

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        for (shot, want) in [(on_key, RED), (on_main, WHITE), (none, WHITE)] {
            let mut cache = ResourceCache::new();
            let mut chains = Chains::default();
            let pixel = draw(&mut compositor, (&mut cache, &mut chains), &shot, 1);
            assert_eq!(pixel, want);
        }
    }

    /// `stats.tick`'s chain cost (ADR-0065 §7) counts a scene's chain as its composite
    /// and one layer and pass per filter, an item's effects as its box and one per
    /// effect, at the sizes they draw at; a source's chain and a bypassed filter add
    /// nothing.
    #[test]
    fn the_chain_cost_counts_scene_chains_and_item_effects() {
        use jstream_protocol::ChainCost;

        assert_eq!(shot(&graph(&[])).chain_cost([]), ChainCost::default());
        assert_eq!(
            shot(&graph(&[("cam", true)])).chain_cost([]),
            ChainCost::default(),
            "a source's own chain is not counted"
        );
        let mut bypassed = graph(&[]);
        apply(&mut bypassed, vec![scene_red("main", false)]);
        assert_eq!(shot(&bypassed).chain_cost([]), ChainCost::default());

        let mut both = graph(&[]);
        let mut second = item_tint("cam", "#00ff00");
        if let Edit::FilterAdd { filter_id, .. } = &mut second {
            *filter_id = FilterId::new("tint_2");
        }
        apply(
            &mut both,
            vec![scene_red("main", true), item_tint("cam", "#ff0000"), second],
        );
        let planned = shot(&both);
        let canvas = planned.canvas().expect("a canvas");
        let canvas_bytes = u64::from(canvas.width()) * u64::from(canvas.height()) * 8;
        let box_bytes = 16 * 9 * 8;
        assert_eq!(
            planned.chain_cost([]),
            ChainCost {
                scene_chains: 1,
                scene_layers: 2,
                scene_passes: 2,
                item_effects: 1,
                item_layers: 3,
                item_passes: 3,
                layer_bytes: 2 * canvas_bytes + 3 * box_bytes,
                ..ChainCost::default()
            },
            "the scene's composite and grade; cam's 16x9 box and its two tints"
        );

        // A chained scene placed twice in another chained scene counts once, and an
        // item drawn by two scenes (main and its keyframe scene) counts once for each.
        let mut nested = graph(&[]);
        let mut edits = vec![
            scene_red("main", true),
            item_tint("cam", "#ff0000"),
            Edit::SceneCreateVariant {
                scene_id: SceneId::new("key"),
                variant_of: SceneId::new("main"),
                name: "Key".into(),
            },
            Edit::SceneCreate {
                scene_id: SceneId::new("outer"),
                name: "Outer".into(),
                role: None,
            },
            scene_red("outer", true),
        ];
        for (source, scene) in [("nest", "main"), ("nest_key", "key")] {
            edits.push(Edit::SourceCreate {
                source_id: SourceId::new(source),
                name: source.into(),
                kind: SourceKind::Scene {
                    scene_id: SceneId::new(scene),
                },
                settings: Default::default(),
                duplicated_from: None,
            });
        }
        for (item, source) in [("a", "nest"), ("b", "nest"), ("c", "nest_key")] {
            edits.push(Edit::ItemAdd {
                scene_id: SceneId::new("outer"),
                item_id: ItemId::new(item),
                source_id: SourceId::new(source),
                transform: None,
                geometry: None,
                opacity: None,
                blend: None,
            });
        }
        apply(&mut nested, edits);
        let planned = shot_of(&nested, "outer");
        let canvas = planned.canvas().expect("a canvas");
        let canvas_bytes = u64::from(canvas.width()) * u64::from(canvas.height()) * 8;
        assert_eq!(
            planned.chain_cost([]),
            ChainCost {
                scene_chains: 3,
                scene_layers: 6,
                scene_passes: 6,
                item_effects: 2,
                item_layers: 4,
                item_passes: 4,
                layer_bytes: 6 * canvas_bytes + 4 * box_bytes,
                ..ChainCost::default()
            },
            "outer, main (placed twice) and key; cam in main and in key, one tint each"
        );
    }

    /// A box with a geometry gets a layer of that box's size in canvas pixels, however
    /// small the frame it shows (ADR-0065 §5).
    #[test]
    fn an_item_effect_belongs_to_its_placement() {
        let mut tinted = graph(&[]);
        let quarter = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(480.0, 270.0).expect("finite"),
            jstream_protocol::Size::try_new(960.0, 540.0).expect("positive"),
        )
        .expect("valid");
        apply(
            &mut tinted,
            vec![
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("cam"),
                    geometry: quarter,
                },
                item_tint("cam", "#ff0000"),
            ],
        );
        let filtered = shot(&tinted);
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let points = [(1, 1), (950, 530), (970, 550)];
        let pixels = draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (&filtered, &LiveParams::default()),
            &points,
        );
        assert_eq!(
            pixels,
            [RED, RED, BLACK],
            "the tinted box covers the top-left quarter, and nothing past it"
        );
        // A live resize (ADR-0032) draws the same layer over the live box.
        let half = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(240.0, 135.0).expect("finite"),
            jstream_protocol::Size::try_new(480.0, 270.0).expect("positive"),
        )
        .expect("valid");
        let live = LiveParams::from_values([(
            None,
            &Edit::ItemSetGeometry {
                scene_id: None,
                item_id: ItemId::new("cam"),
                geometry: half,
            },
        )]);
        let pixels = draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (&filtered, &live),
            &points,
        );
        assert_eq!(pixels, [RED, BLACK, BLACK], "the live box is smaller");
        cache.begin_frame();
        let layer = cache
            .get(&GpuKey::ItemBox(SceneId::new("main"), ItemId::new("cam")))
            .map(|image| (image.width(), image.height()));
        drop(cache.end_frame(&compositor));
        assert_eq!(layer, Some((960, 540)));
    }

    /// A `scene_mask` reads its scene with the scene's own filters, as it shows where
    /// it is placed: `spot` is opaque, but `matte`'s scene filter clears it.
    #[test]
    fn a_scene_mask_reads_a_scene_with_its_filters() {
        let mut graph = masked(100.0);
        apply(
            &mut graph,
            vec![Edit::FilterAdd {
                owner: FilterOwner::Scene {
                    scene_id: SceneId::new("matte"),
                },
                chain: FilterChain::Video,
                filter_id: FilterId::new("clear"),
                kind: "color_correction".into(),
                settings: [("opacity", serde_json::json!(0.0))].into_iter().collect(),
                enabled: Some(true),
                index: None,
            }],
        );
        let cleared = shot(&graph);
        let cam = cleared.chain(&SourceId::new("cam")).expect("cam's chain");
        let read = cam.filters.iter().find_map(|filter| filter.scene.clone());
        assert_eq!(
            read,
            Some(GpuKey::Filtered(ChainOwner::Scene(SceneId::new("matte"))))
        );

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let pixel = draw(&mut compositor, (&mut cache, &mut chains), &cleared, 1);
        assert_eq!(pixel, RED, "cut out where the filtered matte is clear");
    }

    /// Goldens of the `vignette` kind at each level (ADR-0065, #861), as the render
    /// crate's geometry goldens are: small text grids, not images, so they hold on any
    /// conformant adapter. The canvas is 64x36 and black, every source a white 16x9
    /// frame, and the vignette a hard blue edge, so each grid shows where the ellipse
    /// falls. `.` is the canvas, `W` the picture and `B` the vignette. Each sample is the
    /// pixel right of and below its cell's centre, so a grid is not quite symmetric.
    const CANVAS: (u32, u32) = (64, 36);

    /// An item stretched over a box: its ID, centre, size and rotation in degrees.
    type Placed<'a> = (&'a str, (f32, f32), (f32, f32), f32);

    /// A hard blue `vignette` on `owner`'s video chain: the picture is blue where the
    /// ellipse's distance (0 at the centre, 1 at the corners) is over 0.8.
    fn vignette(owner: FilterOwner) -> Edit {
        Edit::FilterAdd {
            owner,
            chain: FilterChain::Video,
            filter_id: FilterId::new("vignette"),
            kind: "vignette".into(),
            settings: [
                ("strength", serde_json::json!(1.0)),
                ("radius", serde_json::json!(0.8)),
                ("softness", serde_json::json!(0.0)),
                ("color", serde_json::json!("#0000ff")),
            ]
            .into_iter()
            .collect(),
            enabled: Some(true),
            index: None,
        }
    }

    /// [`graph`] on the 64x36 canvas with item `id` stretched over the box centred at
    /// `center`, of `size` and turned `rotation_deg`, for each `(id, center, size,
    /// rotation_deg)` in `boxes`.
    fn placed(boxes: &[Placed]) -> Graph {
        let mut graph = graph(&[]);
        // The grids were drawn in linear light, where a glow's faint edge still clears the
        // black class; browser-style blending (ADR-0126) mixes it dimmer, so these pin
        // the space they test the effects' reach in.
        let mut edits = vec![
            Edit::CanvasSetSize {
                width: CANVAS.0,
                height: CANVAS.1,
            },
            Edit::CanvasSetBlending {
                blending: jstream_protocol::Blending::Linear,
            },
        ];
        for &(id, (x, y), (w, h), rotation_deg) in boxes {
            let mut geometry = jstream_protocol::ItemGeometry::try_new(
                jstream_protocol::Vec2::new(x, y).expect("finite"),
                jstream_protocol::Size::try_new(w, h).expect("positive"),
            )
            .expect("valid");
            geometry.fit_mode = jstream_protocol::FitMode::Stretch;
            geometry.rotation_deg = jstream_protocol::Finite32::new(rotation_deg).expect("finite");
            edits.push(Edit::ItemSetGeometry {
                scene_id: None,
                item_id: ItemId::new(id),
                geometry,
            });
        }
        apply(&mut graph, edits);
        graph
    }

    /// Item `id` hidden, so a grid shows only the others.
    fn hide(id: &str) -> Edit {
        Edit::ItemSetVisible {
            scene_id: None,
            item_id: ItemId::new(id),
            visible: false,
        }
    }

    /// `shot`'s program drawn once, sampled every 4 pixels at the centre of each 4x4
    /// cell: `.` black, `W` white, `B` blue, `?` anything more than a few levels from
    /// all three. `None` where the machine has no adapter.
    fn golden(shot: &Shot) -> Option<String> {
        golden_at(shot, 100).map(|(grid, _)| grid)
    }

    /// A layer's size in pixels, then its extent.
    type LayerSize = [(u32, u32); 2];

    /// [`golden`], with 3D models drawn at `percent` of the canvas's width and of its
    /// height (ADR-0197), and the size in pixels and the extent of `cam`'s layer.
    fn golden_at(shot: &Shot, percent: u32) -> Option<(String, Option<LayerSize>)> {
        const PALETTE: [(char, [u8; 3]); 3] =
            [('.', [0, 0, 0]), ('W', [255, 255, 255]), ('B', [0, 0, 255])];
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let mut compositor = gpu()?;
        let mut cache = ResourceCache::new();
        let mut chains = Chains {
            model_percent: crate::resolution::Percent::of(percent),
            ..Chains::default()
        };
        let (width, height) = CANVAS;
        let points: Vec<_> = (0..height / 4)
            .flat_map(|y| (0..width / 4).map(move |x| (x * 4 + 2, y * 4 + 2)))
            .collect();
        let pixels = draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (shot, &LiveParams::default()),
            &points,
        );
        let mut out = String::new();
        for row in pixels.chunks((width / 4) as usize) {
            for pixel in row {
                let class = PALETTE
                    .iter()
                    .find(|(_, rgb)| (0..3).all(|c| pixel[c].abs_diff(rgb[c]) <= 4))
                    .map_or('?', |(name, _)| *name);
                out.push(class);
            }
            out.push('\n');
        }
        let layer = cache
            .get(&GpuKey::ItemBox(SceneId::new("main"), ItemId::new("cam")))
            .map(|layer| [(layer.width(), layer.height()), layer.extent()]);
        Some((out, layer))
    }

    /// Asserts `shot`'s [`golden`] is `expected`, a grid written one row per line,
    /// where the machine has an adapter.
    fn assert_golden(shot: &Shot, expected: &str, what: &str) {
        let Some(actual) = golden(shot) else { return };
        let expected: String = expected
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .flat_map(|line| line.chars().chain(['\n']))
            .collect();
        assert_eq!(actual, expected, "{what}\nactual:\n{actual}");
    }

    /// [`shot`], with `cam` showing an unlit white model of `shape` (ADR-0107) in place
    /// of its frame, a transparent stand-in.
    fn model_shot(graph: &Graph, shape: &str) -> Shot {
        model_shot_with(graph, shape, |shown| shown)
    }

    /// [`model_shot`], with the shown model changed by `change` first.
    fn model_shot_with(
        graph: &Graph,
        shape: &str,
        change: impl Fn(jstream_sources::ShownModel) -> jstream_sources::ShownModel,
    ) -> Shot {
        use jstream_sources::SourceKind as _;
        let kind = jstream_sources::ModelKind::new();
        let mut settings: jstream_protocol::Settings = kind
            .descriptor()
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect();
        settings.insert(String::from("shape"), serde_json::json!(shape));
        settings.insert(String::from("lighting"), serde_json::json!("unlit"));
        settings.insert(String::from("color"), serde_json::json!("#ffffff"));
        let source = kind
            .create(&settings, &jstream_sources::Context::default())
            .expect("a model");
        let shown = change(source.model().expect("a shape is a model"));
        let white: Vec<u8> = std::iter::repeat_n(WHITE, 16 * 9).flatten().collect();
        let frame = Frame::new(16, 9, &white, 1).expect("invariant: 16x9 RGBA8");
        let stand_in = Frame::new(16, 9, &[0; 16 * 9 * 4], 2)
            .expect("invariant: 16x9 RGBA8")
            .with_model(Some(shown));
        snapshot::plan_with(
            graph,
            Some(&SceneId::new("main")),
            None,
            &mut |id| {
                Some(Look::frame(
                    if id.as_str() == "cam" {
                        stand_in.clone()
                    } else {
                        frame.clone()
                    },
                    None,
                ))
            },
            &FilterAssets::default(),
        )
        .shot
    }

    /// A rigged model is drawn as its pose places it (ADR-0151 decision 7), in a box
    /// fitted to its rest pose: a pose half its width to the right moves it there, in
    /// its layer, without rescaling it.
    #[test]
    fn golden_a_posed_model_draws_where_its_pose_places_it() {
        use jstream_media::model::rig::{Mat4, Rest, Rig, RigNode, Slot};
        let mut graph = placed(&[("cam", (16.0, 18.0), (32.0, 36.0), 0.0)]);
        apply(&mut graph, vec![hide("logo")]);
        let identity = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let rigged = |palette: Mat4| {
            move |shown: jstream_sources::ShownModel| {
                let model = &shown.model;
                let mut primitives = model.primitives().to_vec();
                for v in primitives.iter_mut().flat_map(|p| &mut p.vertices) {
                    v.weights = [1.0, 0.0, 0.0, 0.0];
                }
                let rig = Rig::new(
                    vec![RigNode {
                        parent: None,
                        rest: Rest::Matrix(identity),
                    }],
                    vec![Slot {
                        node: 0,
                        inverse_bind: identity,
                    }],
                    vec![],
                )
                .expect("a rig");
                let model = jstream_media::model::Model::rigged(
                    primitives,
                    model.materials().to_vec(),
                    model.textures().to_vec(),
                    Some(std::sync::Arc::new(rig)),
                )
                .expect("a model");
                jstream_sources::ShownModel {
                    model: std::sync::Arc::new(model),
                    pose: Some(vec![palette].into()),
                    ..shown
                }
            }
        };
        let rest = identity.map(|v| v as f32);
        assert_golden(
            &model_shot_with(&graph, "plane", rigged(rest)),
            "
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            ",
            "at rest the plane fills its box",
        );
        // Half a unit to the right: half the box.
        let mut moved = rest;
        moved[12] = 0.5;
        assert_golden(
            &model_shot_with(&graph, "plane", rigged(moved)),
            "
            ....WWWWWWWW....
            ....WWWWWWWW....
            ....WWWWWWWW....
            ....WWWWWWWW....
            ....WWWWWWWW....
            ....WWWWWWWW....
            ....WWWWWWWW....
            ....WWWWWWWW....
            ....WWWWWWWW....
            ",
            "the pose moves it",
        );
    }

    /// ADR-0177 decisions 1 and 2: with a thread for the uploads, a draw asks for a
    /// model's upload and doesn't make it. The item draws nothing until the upload is
    /// back, and a model that changed is drawn as it was until the new one is.
    #[test]
    fn a_model_uploaded_aside_draws_once_its_upload_is_back() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        chains.upload_models_aside(&compositor);
        let mut graph = placed(&[("cam", (16.0, 18.0), (32.0, 36.0), 0.0)]);
        apply(&mut graph, vec![hide("logo")]);
        let live = LiveParams::default();
        let middle = [(16, 18)];
        // Draws `shot` until its middle is `want`, each draw a frame of its own.
        fn draw_until(
            compositor: &mut Compositor,
            (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
            shot: &Shot,
            want: [u8; 4],
        ) {
            let live = LiveParams::default();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                let drawn = draw_points(
                    compositor,
                    (&mut *cache, &mut *chains),
                    (shot, &live),
                    &[(16, 18)],
                );
                if drawn[0] == want {
                    return;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "never drew {want:?}: {:?}",
                    drawn[0]
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }

        let white = model_shot(&graph, "plane");
        let first = draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (&white, &live),
            &middle,
        );
        assert_eq!(first[0], BLACK, "nothing is drawn in the frame that asks");
        assert!(chains.models_coming(), "the thread was asked");
        draw_until(&mut compositor, (&mut cache, &mut chains), &white, WHITE);
        assert!(!chains.models_coming());

        // The same plane in blue is another model, so another version.
        let blue = model_shot_with(&graph, "plane", |shown| {
            let model = &shown.model;
            let materials = model
                .materials()
                .iter()
                .map(|m| jstream_media::model::Material {
                    base_color: [0.0, 0.0, 1.0, 1.0],
                    ..m.clone()
                })
                .collect();
            let model = jstream_media::model::Model::new(
                model.primitives().to_vec(),
                materials,
                model.textures().to_vec(),
            )
            .expect("a model");
            jstream_sources::ShownModel {
                model: std::sync::Arc::new(model),
                ..shown
            }
        });
        let changed = draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (&blue, &live),
            &middle,
        );
        assert_eq!(changed[0], WHITE, "the version before is drawn meanwhile");
        assert!(chains.models_coming());
        draw_until(
            &mut compositor,
            (&mut cache, &mut chains),
            &blue,
            [0, 0, 255, 255],
        );
    }

    /// ADR-0177 decision 5: a model a warm scene's source shows is uploaded and kept
    /// though no frame draws it, and let go of once it is no longer kept.
    #[test]
    fn a_kept_model_stays_uploaded_though_no_frame_draws_it() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let live = LiveParams::default();
        let plain = shot(&graph(&[]));
        let cube = SourceId::new("cube");
        let model = jstream_media::model::shapes::Shape::Cube
            .model(jstream_media::model::Material::default())
            .expect("a cube");
        let kept: std::collections::BTreeMap<_, _> =
            [(cube.clone(), std::sync::Arc::new(model))].into();
        for _ in 0..3 {
            chains.keep_models(&compositor, &kept);
            draw_points(
                &mut compositor,
                (&mut cache, &mut chains),
                (&plain, &live),
                &[(2, 2)],
            );
            let held = chains.model(&cube).expect("the kept model is uploaded");
            assert_eq!(held.version(), kept[&cube].version());
        }
        for _ in 0..2 {
            draw_points(
                &mut compositor,
                (&mut cache, &mut chains),
                (&plain, &live),
                &[(2, 2)],
            );
        }
        assert!(chains.model(&cube).is_none(), "let go of once not kept");
    }

    /// A model item draws its model where its box is, posed by its geometry and moved by
    /// its motion (ADR-0107 decision 6), with or without effects over its layer.
    #[test]
    fn golden_a_model_draws_in_its_box_and_moves_with_its_motion() {
        let mut graph = placed(&[("cam", (16.0, 18.0), (32.0, 36.0), 0.0)]);
        apply(&mut graph, vec![hide("logo")]);
        assert_golden(
            &model_shot(&graph, "plane"),
            "
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            ",
            "the plane fills its box",
        );
        // An oscillator a quarter through its period is at its amplitude: 32 px right.
        apply(
            &mut graph,
            vec![Edit::ItemSetMotion {
                item_id: ItemId::new("cam"),
                scene_id: None,
                motion: vec![
                    serde_json::from_value(serde_json::json!({
                        "modifier_id": "slide",
                        "property": "x",
                        "kind": "oscillate",
                        "amplitude": 32,
                        "period_ms": 3600000,
                        "phase": 0.25,
                    }))
                    .expect("a modifier"),
                ],
            }],
        );
        assert_golden(
            &model_shot(&graph, "plane"),
            "
            ........WWWWWWWW
            ........WWWWWWWW
            ........WWWWWWWW
            ........WWWWWWWW
            ........WWWWWWWW
            ........WWWWWWWW
            ........WWWWWWWW
            ........WWWWWWWW
            ........WWWWWWWW
            ",
            "its motion moves the model",
        );
        apply(
            &mut graph,
            vec![vignette(FilterOwner::Item {
                item_id: ItemId::new("cam"),
            })],
        );
        assert_golden(
            &model_shot(&graph, "plane"),
            "
            ........WWWWWWBB
            ........WWWWWWWB
            ........WWWWWWWW
            ........WWWWWWWW
            ........WWWWWWWW
            ........WWWWWWWW
            ........WWWWWWWW
            ........WWWWWWWB
            ........WWWWWBBB
            ",
            "effects run over the model's layer: the canvas's ellipse, cut to the model",
        );
    }

    /// A model's `texture` (ADR-0107 decision 5) shows another source's picture on its
    /// surfaces, after that source's own filters, though no item shows the source; a
    /// texture for another material leaves it as it was, and one naming a source that
    /// shows nothing reads transparent.
    #[test]
    fn golden_a_model_shows_another_sources_picture_as_its_texture() {
        let mut graph = placed(&[("cam", (16.0, 18.0), (32.0, 36.0), 0.0)]);
        apply(
            &mut graph,
            vec![
                hide("logo"),
                Edit::FilterAdd {
                    owner: FilterOwner::Source {
                        source_id: SourceId::new("logo"),
                    },
                    chain: FilterChain::Video,
                    filter_id: FilterId::new("blue"),
                    kind: "color_correction".into(),
                    settings: [("multiply", serde_json::json!("#0000ff"))]
                        .into_iter()
                        .collect(),
                    enabled: Some(true),
                    index: None,
                },
            ],
        );
        let textured = |source: &'static str, material: Option<&'static str>| {
            move |shown: jstream_sources::ShownModel| jstream_sources::ShownModel {
                textures: vec![jstream_sources::ShownTexture {
                    source: SourceId::new(source),
                    on: material.map(str::to_owned),
                }],
                ..shown
            }
        };
        // Drawn once: the texture's chain must run before the model's layer is drawn.
        let shot = model_shot_with(&graph, "plane", textured("logo", None));
        assert_golden(
            &shot,
            "
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            ",
            "the plane shows the filtered source",
        );
        assert_golden(
            &model_shot_with(&graph, "plane", textured("logo", Some("not-a-material"))),
            "
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            ",
            "a texture for another material leaves the shape's own",
        );
        assert_golden(
            &model_shot_with(&graph, "plane", textured("missing", None)),
            "
            ................
            ................
            ................
            ................
            ................
            ................
            ................
            ................
            ................
            ",
            "a missing source reads transparent: the opaque plane is black",
        );
        // A scene source's scene, drawn though nothing else shows it.
        let mut cover = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(64.0, 36.0).expect("positive"),
        )
        .expect("valid");
        cover.fit_mode = jstream_protocol::FitMode::Stretch;
        apply(
            &mut graph,
            vec![
                Edit::SceneCreate {
                    scene_id: SceneId::new("inner"),
                    name: "Inner".into(),
                    role: None,
                },
                Edit::ItemAdd {
                    scene_id: SceneId::new("inner"),
                    item_id: ItemId::new("inner-logo"),
                    source_id: SourceId::new("logo"),
                    transform: None,
                    geometry: Some(cover),
                    opacity: None,
                    blend: None,
                },
                Edit::SourceCreate {
                    source_id: SourceId::new("inner-view"),
                    name: "Inner".into(),
                    kind: SourceKind::Scene {
                        scene_id: SceneId::new("inner"),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
            ],
        );
        assert_golden(
            &model_shot_with(&graph, "plane", textured("inner-view", None)),
            "
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            ",
            "the plane shows the scene source's scene",
        );
    }

    /// ADR-0107 decision 7: in a 3D scene a card drawn after a model but behind it is
    /// hidden where the model is nearer; without a camera there is no depth, and the
    /// card is drawn over the model as stacking says.
    #[test]
    fn golden_a_card_behind_a_model_is_hidden_where_the_model_is_nearer() {
        let mut graph = placed(&[
            ("cam", (32.0, 18.0), (16.0, 16.0), 0.0),
            ("logo", (32.0, 18.0), (64.0, 36.0), 0.0),
        ]);
        let mut near = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(16.0, 16.0).expect("positive"),
        )
        .expect("valid");
        near.fit_mode = jstream_protocol::FitMode::Stretch;
        near.depth_px = jstream_protocol::Finite32::new(10.0).expect("finite");
        apply(
            &mut graph,
            vec![
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("cam"),
                    geometry: near,
                },
                Edit::ItemReorder {
                    item_id: ItemId::new("cam"),
                    index: 0,
                },
                Edit::FilterAdd {
                    owner: FilterOwner::Source {
                        source_id: SourceId::new("logo"),
                    },
                    chain: FilterChain::Video,
                    filter_id: FilterId::new("blue"),
                    kind: "color_correction".into(),
                    settings: [("multiply", serde_json::json!("#0000ff"))]
                        .into_iter()
                        .collect(),
                    enabled: Some(true),
                    index: None,
                },
            ],
        );
        // The model is under the card in stacking order.
        let flat = "
            BBBBBBBBBBBBBBBB
            BBBBBBBBBBBBBBBB
            BBBBBBBBBBBBBBBB
            BBBBBBBBBBBBBBBB
            BBBBBBBBBBBBBBBB
            BBBBBBBBBBBBBBBB
            BBBBBBBBBBBBBBBB
            BBBBBBBBBBBBBBBB
            BBBBBBBBBBBBBBBB
            ";
        assert_golden(
            &model_shot(&graph, "plane"),
            flat,
            "2D: the card is over the model",
        );
        let mut camera = jstream_protocol::SceneCamera::framing(jstream_protocol::Canvas {
            width: CANVAS.0,
            height: CANVAS.1,
        });
        // Stacking order, so only the model's depth can put it in front.
        camera.depth_sort = false;
        apply(
            &mut graph,
            vec![Edit::SceneSetCamera {
                scene_id: SceneId::new("main"),
                camera: Some(camera),
            }],
        );
        assert_golden(
            &model_shot(&graph, "plane"),
            "
            BBBBBBBBBBBBBBBB
            BBBBBBBBBBBBBBBB
            BBBBBWWWWWBBBBBB
            BBBBBWWWWWBBBBBB
            BBBBBWWWWWBBBBBB
            BBBBBWWWWWBBBBBB
            BBBBBWWWWWBBBBBB
            BBBBBBBBBBBBBBBB
            BBBBBBBBBBBBBBBB
            ",
            "3D: the nearer model hides the card behind it",
        );
    }

    /// ADR-0107 decision 5: a model whose `texture` or `reflection` reaches a scene that
    /// shows the model is a cycle, refused as a scene source's is; another scene is not.
    #[test]
    fn a_models_texture_reaching_its_own_scene_is_a_cycle() {
        let reference = |id: &str| {
            jstream_protocol::Reference::Source {
                source_id: SourceId::new(id),
            }
            .to_value()
        };
        let mut graph = Graph::new().with_kinds(crate::kinds::builtin());
        let scene = |id: &str| Edit::SceneCreate {
            scene_id: SceneId::new(id),
            name: id.into(),
            role: None,
        };
        let source = |id: &str, kind: SourceKind, settings: Settings| Edit::SourceCreate {
            source_id: SourceId::new(id),
            name: id.into(),
            kind,
            settings,
            duplicated_from: None,
        };
        graph
            .apply(
                &[
                    scene("main"),
                    scene("other"),
                    source(
                        "model",
                        SourceKind::Builtin {
                            kind: "model".into(),
                        },
                        [("shape", serde_json::json!("cube"))].into_iter().collect(),
                    ),
                    Edit::ItemAdd {
                        scene_id: SceneId::new("main"),
                        item_id: ItemId::new("m"),
                        source_id: SourceId::new("model"),
                        transform: None,
                        geometry: None,
                        opacity: None,
                        blend: None,
                    },
                    source(
                        "main-view",
                        SourceKind::Scene {
                            scene_id: SceneId::new("main"),
                        },
                        Settings::default(),
                    ),
                    source(
                        "other-view",
                        SourceKind::Scene {
                            scene_id: SceneId::new("other"),
                        },
                        Settings::default(),
                    ),
                ],
                None,
            )
            .expect("the sources are made");
        for param in ["texture", "reflection"] {
            let set = |to: &str| Edit::SourceSetSettings {
                source_id: SourceId::new("model"),
                patch: [(param, reference(to))].into_iter().collect(),
            };
            let refused = graph.apply(&[set("main-view")], None);
            assert!(
                matches!(&refused, Err(jstream_protocol::ProtocolError::InvalidArgument { reason, .. }) if reason.contains("cycle")),
                "{param}: {refused:?}"
            );
            graph
                .apply(&[set("other-view")], None)
                .expect("another scene is no cycle");
        }
    }

    /// The centre pixel of `graph` with `cam` a white `scene`-lit plane (ADR-0143
    /// decision 3) and `logo` the light `light`, or `None` without an adapter.
    fn lit_centre(graph: &Graph, light: jstream_sources::ShownLight) -> Option<[u8; 4]> {
        lit_centre_in(graph, light, None)
    }

    /// [`lit_centre`] with `running` under way at its start.
    fn lit_centre_in(
        graph: &Graph,
        light: jstream_sources::ShownLight,
        running: Option<&crate::transition::Running>,
    ) -> Option<[u8; 4]> {
        use jstream_sources::SourceKind as _;
        let kind = jstream_sources::ModelKind::new();
        let mut settings: jstream_protocol::Settings = kind
            .descriptor()
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect();
        settings.insert(String::from("shape"), serde_json::json!("plane"));
        settings.insert(String::from("lighting"), serde_json::json!("scene"));
        settings.insert(String::from("color"), serde_json::json!("#ffffff"));
        let source = kind
            .create(&settings, &jstream_sources::Context::default())
            .expect("a model");
        let stand_in = Frame::new(16, 9, &[0; 16 * 9 * 4], 2)
            .expect("invariant: 16x9 RGBA8")
            .with_model(source.model());
        let lamp = Frame::new(1, 1, &[0; 4], 3)
            .expect("invariant: 1x1 RGBA8")
            .with_light(Some(light));
        let shot = snapshot::plan_with(
            graph,
            Some(&SceneId::new("main")),
            running.map(|running| (running, running.start)),
            &mut |id| {
                Some(Look::frame(
                    if id.as_str() == "cam" {
                        stand_in.clone()
                    } else {
                        lamp.clone()
                    },
                    None,
                ))
            },
            &FilterAssets::default(),
        )
        .shot;
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let mut compositor = gpu()?;
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let pixels = draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (&shot, &LiveParams::default()),
            &[(32, 18)],
        );
        Some(pixels[0])
    }

    /// ADR-0170 decisions 2 and 3: a mirror plane in a scene whose `reflections` is on,
    /// inside a reflection probe's box, reflects a red card behind the camera, which the
    /// camera can't see but the probe does, from the frame after the probe first
    /// captures. With the scene's reflections off, or the switch off, it doesn't.
    #[test]
    fn a_mirror_inside_a_probe_reflects_what_is_behind_the_camera() {
        use jstream_sources::SourceKind as _;
        let mut graph = placed(&[
            ("cam", (32.0, 18.0), (32.0, 36.0), 0.0),
            ("logo", (32.0, 18.0), (4.0, 4.0), 0.0),
        ]);
        let mut behind = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(400.0, 400.0).expect("positive"),
        )
        .expect("valid");
        behind.fit_mode = jstream_protocol::FitMode::Stretch;
        behind.depth_px = jstream_protocol::Finite32::new(200.0).expect("finite");
        let camera = jstream_protocol::SceneCamera::framing(jstream_protocol::Canvas {
            width: CANVAS.0,
            height: CANVAS.1,
        });
        apply(
            &mut graph,
            vec![
                Edit::SceneSetCamera {
                    scene_id: SceneId::new("main"),
                    camera: Some(camera),
                },
                Edit::SourceCreate {
                    source_id: SourceId::new("back"),
                    name: "back".into(),
                    kind: SourceKind::Builtin {
                        kind: "display".into(),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
                Edit::ItemAdd {
                    scene_id: SceneId::new("main"),
                    item_id: ItemId::new("back"),
                    source_id: SourceId::new("back"),
                    transform: None,
                    geometry: Some(behind),
                    opacity: None,
                    blend: None,
                },
            ],
        );
        let reflective = |on: bool| Edit::SceneSetOptions {
            scene_id: SceneId::new("main"),
            options: jstream_protocol::SceneOptions {
                reflections: on,
                ..jstream_protocol::SceneOptions::default()
            },
        };
        apply(&mut graph, vec![reflective(true)]);
        let kind = jstream_sources::ModelKind::new();
        let mut settings: jstream_protocol::Settings = kind
            .descriptor()
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect();
        settings.insert(String::from("shape"), serde_json::json!("plane"));
        settings.insert(String::from("lighting"), serde_json::json!("scene"));
        settings.insert(String::from("color"), serde_json::json!("#ffffff"));
        settings.insert(String::from("metallic"), serde_json::json!(1.0));
        settings.insert(String::from("roughness"), serde_json::json!(0.0));
        let model = kind
            .create(&settings, &jstream_sources::Context::default())
            .expect("a model");
        let stand_in = Frame::new(16, 9, &[0; 16 * 9 * 4], 2)
            .expect("invariant: 16x9 RGBA8")
            .with_model(model.model());
        let probe = Frame::new(1, 1, &[0; 4], 3)
            .expect("invariant: 1x1 RGBA8")
            .with_probe(Some(jstream_sources::ShownProbe {
                size_px: [10_000.0; 3],
                fade_px: 0.0,
                resolution: 128,
                refresh_frames: 0,
            }));
        let red = Frame::new(4, 4, &[255, 0, 0, 255].repeat(16), 4).expect("invariant: 4x4 RGBA8");
        let centre = |graph: &Graph, features: jstream_protocol::RenderFeatures| {
            let assets = FilterAssets::default().with_features(features);
            let shot = snapshot::plan_with(
                graph,
                Some(&SceneId::new("main")),
                None,
                &mut |id| {
                    Some(Look::frame(
                        match id.as_str() {
                            "cam" => stand_in.clone(),
                            "back" => red.clone(),
                            _ => probe.clone(),
                        },
                        None,
                    ))
                },
                &assets,
            )
            .shot;
            let _gpu = crate::tests::one_gpu_test_at_a_time();
            let mut compositor = gpu()?;
            let mut cache = ResourceCache::new();
            let mut chains = Chains::default();
            let live = LiveParams::default();
            // The first six frames each draw a face and the sixth resamples them; the
            // seventh reads the capture.
            let mut frames = (0..7).map(|_| {
                draw_points(
                    &mut compositor,
                    (&mut cache, &mut chains),
                    (&shot, &live),
                    &[(32, 18)],
                )[0]
            });
            let first = frames.next()?;
            Some((first, frames.last()?))
        };
        let all = jstream_protocol::RenderFeatures::ALL;
        let Some((before, mirrored)) = centre(&graph, all) else {
            return;
        };
        assert!(
            mirrored[0] > mirrored[1] + 60 && mirrored[0] > mirrored[2] + 60,
            "the mirror shows the red card: {mirrored:?} (before the capture {before:?})"
        );
        assert!(
            before[0] < mirrored[0],
            "nothing before the first capture: {before:?}"
        );
        let off = jstream_protocol::RenderFeatures {
            reflection_probes: false,
            ..all
        };
        let (_, switched) = centre(&graph, off).expect("an adapter");
        assert_eq!(switched, before, "the switch off reflects the environment");
        apply(&mut graph, vec![reflective(false)]);
        let (_, plain) = centre(&graph, all).expect("an adapter");
        assert_eq!(
            plain, before,
            "a scene without reflections reflects the environment"
        );
    }

    /// ADR-0170 decision 3: the one capture under way goes to the due probe that has
    /// waited longest, so a probe due every frame, first in its scene, can't keep the
    /// probe after it from ever capturing.
    #[test]
    fn a_probe_due_every_frame_starves_no_other() {
        let mut graph = placed(&[
            ("logo", (32.0, 18.0), (4.0, 4.0), 0.0),
            ("cam", (20.0, 18.0), (4.0, 4.0), 0.0),
        ]);
        apply(
            &mut graph,
            vec![
                Edit::SceneSetCamera {
                    scene_id: SceneId::new("main"),
                    camera: Some(jstream_protocol::SceneCamera::framing(
                        jstream_protocol::Canvas {
                            width: CANVAS.0,
                            height: CANVAS.1,
                        },
                    )),
                },
                Edit::SceneSetOptions {
                    scene_id: SceneId::new("main"),
                    options: jstream_protocol::SceneOptions {
                        reflections: true,
                        ..jstream_protocol::SceneOptions::default()
                    },
                },
            ],
        );
        let probe = |refresh_frames: u32| {
            Frame::new(1, 1, &[0; 4], 3)
                .expect("invariant: 1x1 RGBA8")
                .with_probe(Some(jstream_sources::ShownProbe {
                    size_px: [1_000.0; 3],
                    fade_px: 0.0,
                    resolution: 128,
                    refresh_frames,
                }))
        };
        // `logo` is first in stacking order and due every frame.
        let (every, once) = (probe(1), probe(0));
        let assets = FilterAssets::default();
        let shot = snapshot::plan_with(
            &graph,
            Some(&SceneId::new("main")),
            None,
            &mut |id| {
                Some(Look::frame(
                    if id.as_str() == "logo" {
                        every.clone()
                    } else {
                        once.clone()
                    },
                    None,
                ))
            },
            &assets,
        )
        .shot;
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else {
            return;
        };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let live = LiveParams::default();
        let mut captured = Vec::new();
        for _ in 0..18 {
            draw_points(
                &mut compositor,
                (&mut cache, &mut chains),
                (&shot, &live),
                &[(32, 18)],
            );
            captured.push(
                chains
                    .probes
                    .keys()
                    .map(|(_, item)| item.as_str().to_owned())
                    .collect::<Vec<_>>(),
            );
        }
        // A capture draws a face a frame, so the first takes frames 0 to 5.
        assert!(
            captured[..5].iter().all(Vec::is_empty),
            "nothing is captured before the sixth face: {captured:?}"
        );
        assert_eq!(
            captured[5],
            ["logo"],
            "the first capture is of the first probe"
        );
        assert_eq!(
            captured[11],
            ["cam", "logo"],
            "the second goes to the one that hasn't tried"
        );
        assert_eq!(captured[17], ["cam", "logo"], "both stay captured");
    }

    /// ADR-0151 decision 9: an environment light lights a `scene`-lit model with its sky
    /// on its own: a blue horizon tints a plane facing the camera blue, and a source
    /// named as its picture, which no item shows, lights it from that picture instead.
    #[test]
    fn an_environment_light_lights_a_scene_lit_model_with_its_sky() {
        let mut graph = placed(&[("cam", (32.0, 18.0), (32.0, 36.0), 0.0)]);
        apply(
            &mut graph,
            vec![Edit::SceneSetCamera {
                scene_id: SceneId::new("main"),
                camera: Some(jstream_protocol::SceneCamera::framing(
                    jstream_protocol::Canvas {
                        width: CANVAS.0,
                        height: CANVAS.1,
                    },
                )),
            }],
        );
        let blue = [0.0, 0.0, 1.0];
        let sky = |image: Option<&str>| jstream_sources::ShownLight {
            kind: jstream_sources::LightType::Environment,
            color: [1.0; 3],
            intensity: 2.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows: false,
            shadow_softness: 0.3,
            environment: Some(jstream_sources::ShownEnvironment {
                sky: blue,
                horizon: blue,
                ground: blue,
                image: image.map(SourceId::new),
                backdrop: false,
                stars: 0.0,
                star_brightness: 1.0,
                nebula: 0.0,
                nebula_colors: [[0.0; 3]; 2],
                galaxy: 0.0,
                sky_seed: 0,
                warp: 0.0,
                warp_ramp_s: 0.0,
                warp_aim: jstream_sources::WarpAim::Sky,
                warp_look_deg: 10.0,
                sky_pictures: None,
                sky_picture_deg: 30.0,
                sky_picture_light: 1.0,
            }),
        };
        let Some(tinted) = lit_centre(&graph, sky(None)) else {
            return;
        };
        assert!(
            tinted[2] > 60 && tinted[2] > tinted[0] + 40,
            "the sky lights the plane blue: {tinted:?}"
        );
        // The picture: a red source that only the light names.
        use jstream_sources::SourceKind as _;
        let kind = jstream_sources::ModelKind::new();
        let mut settings: jstream_protocol::Settings = kind
            .descriptor()
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect();
        settings.insert(String::from("shape"), serde_json::json!("plane"));
        settings.insert(String::from("lighting"), serde_json::json!("scene"));
        settings.insert(String::from("color"), serde_json::json!("#ffffff"));
        let model = kind
            .create(&settings, &jstream_sources::Context::default())
            .expect("a model");
        let stand_in = Frame::new(16, 9, &[0; 16 * 9 * 4], 2)
            .expect("invariant: 16x9 RGBA8")
            .with_model(model.model());
        let lamp = Frame::new(1, 1, &[0; 4], 3)
            .expect("invariant: 1x1 RGBA8")
            .with_light(Some(sky(Some("panorama"))));
        let red = Frame::new(4, 2, &[255, 0, 0, 255].repeat(8), 4).expect("invariant: 4x2 RGBA8");
        let shot = snapshot::plan_with(
            &graph,
            Some(&SceneId::new("main")),
            None,
            &mut |id| {
                Some(Look::frame(
                    match id.as_str() {
                        "cam" => stand_in.clone(),
                        "panorama" => red.clone(),
                        _ => lamp.clone(),
                    },
                    None,
                ))
            },
            &FilterAssets::default(),
        )
        .shot;
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let mut compositor = gpu().expect("an adapter");
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let pixel = draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (&shot, &LiveParams::default()),
            &[(32, 18)],
        )[0];
        assert!(
            pixel[0] > 60 && pixel[0] > pixel[2] + 40,
            "the picture lights the plane red: {pixel:?}"
        );
    }

    /// ADR-0167: a backdrop's travel advances by its rate times the media time between
    /// frames, once per cache frame, and a seek or a long pause moves nothing.
    #[test]
    fn a_sky_travels_at_its_rate_once_a_frame() {
        let rate = jstream_render::SkyDraw::travel_rate;
        let mut sky = crate::chain::SkyState::default();
        sky.step(1, 10.0, (1.0, 0.0), SKY_AIM, (0, 0));
        assert_eq!(
            (sky.travel, sky.warp),
            (0.0, 1.0),
            "the first frame starts at its warp"
        );
        sky.step(2, 10.5, (1.0, 0.0), SKY_AIM, (0, 0));
        assert!((sky.travel - rate(1.0) * 0.5).abs() < 1e-9);
        sky.step(2, 10.9, (1.0, 0.0), SKY_AIM, (0, 0));
        assert!(
            (sky.travel - rate(1.0) * 0.5).abs() < 1e-9,
            "once per frame"
        );
        let before = sky.travel;
        sky.step(3, 30.0, (0.0, 2.0), SKY_AIM, (0, 0));
        assert_eq!(
            (sky.travel, sky.warp),
            (before, 0.0),
            "a jump moves nothing"
        );
        // Ramping from 0 toward 1 over 2 s: half a second in, the warp is a quarter.
        sky.step(4, 30.5, (1.0, 2.0), SKY_AIM, (0, 0));
        assert!((sky.warp - 0.25).abs() < 1e-6, "{}", sky.warp);
        let ramped = before + (rate(0.0) + rate(0.25)) * 0.5 * 0.5;
        assert!((sky.travel - ramped).abs() < 1e-6);
        sky.step(5, 29.0, (1.0, 2.0), SKY_AIM, (0, 0));
        assert!(
            (sky.travel - ramped).abs() < 1e-6,
            "back in time moves nothing"
        );
        // A held clock: frames at one time keep the warp where it is.
        sky.step(50, 40.0, (0.0, 0.0), SKY_AIM, (0, 0));
        sky.step(51, 40.0, (1.0, 2.0), SKY_AIM, (0, 0));
        sky.step(52, 40.0, (1.0, 2.0), SKY_AIM, (0, 0));
        assert_eq!(sky.warp, 0.0, "no time, no ramp and no jump");
        sky.step(53, 40.5, (1.0, 2.0), SKY_AIM, (0, 0));
        assert!((sky.warp - 0.25).abs() < 1e-6, "{}", sky.warp);
        sky.step(5, 29.0, (1.0, 2.0), SKY_AIM, (0, 0));
        assert_eq!(sky.dropping, 0.0, "at its setting, it isn't dropping");
        // Setting it lower: dropping until the warp gets there.
        sky.step(6, 29.5, (0.0, 2.0), SKY_AIM, (0, 0));
        assert!(
            sky.dropping == 1.0 && (sky.warp - 0.75).abs() < 1e-6,
            "{}",
            sky.warp
        );
        sky.step(7, 30.5, (0.0, 2.0), SKY_AIM, (0, 0));
        assert!((sky.warp - 0.25).abs() < 1e-6 && sky.dropping == 1.0);
        // Toward 0 too it eases out over the last 0.15 (ADR-0186).
        sky.step(70, 30.7, (0.0, 2.0), SKY_AIM, (0, 0));
        assert!(
            (sky.warp - 0.15).abs() < 1e-5 && (sky.dropping - 1.0).abs() < 1e-4,
            "{}",
            sky.dropping
        );
        sky.step(71, 30.85, (0.0, 2.0), SKY_AIM, (0, 0));
        assert!((sky.dropping - 0.5).abs() < 1e-3, "{}", sky.dropping);
        sky.step(8, 31.0, (0.0, 2.0), SKY_AIM, (0, 0));
        assert!(sky.warp == 0.0 && sky.dropping == 0.0, "it arrives");
        // A setting that falls to 0 under the warp moves it a little, never in a jump:
        // a modifier's envelope decaying to nothing (ADR-0186).
        sky.step(80, 40.0, (0.12, 2.0), SKY_AIM, (0, 0));
        sky.step(81, 40.01, (0.01, 2.0), SKY_AIM, (0, 0));
        let before = sky.dropping;
        assert!((before - 0.7).abs() < 1e-3, "{before}");
        sky.step(82, 40.02, (0.0, 2.0), SKY_AIM, (0, 0));
        assert!(
            (sky.dropping - 0.11 / 0.15).abs() < 1e-3,
            "{before} to {}",
            sky.dropping
        );
        // Toward a cruise above 0 it eases out over the last 0.15.
        sky.step(9, 31.5, (1.0, 0.0), SKY_AIM, (0, 0));
        sky.step(10, 32.0, (0.1, 4.0), SKY_AIM, (0, 0));
        assert!((sky.dropping - 1.0).abs() < 1e-6);
        sky.step(11, 33.0, (0.1, 4.0), SKY_AIM, (0, 0));
        sky.step(12, 34.0, (0.1, 4.0), SKY_AIM, (0, 0));
        sky.step(13, 34.9, (0.1, 4.0), SKY_AIM, (0, 0));
        assert!(
            (sky.warp - 0.15).abs() < 1e-5 && (sky.dropping - 1.0 / 3.0).abs() < 1e-4,
            "{}",
            sky.dropping
        );
    }

    /// A sky whose warp travels toward its own centre, its item unturned.
    const SKY_AIM: crate::chain::WarpAiming = crate::chain::WarpAiming {
        angles: [0.0; 3],
        ahead: None,
    };

    /// ADR-0187: a warp aimed ahead lays the sky out around straight ahead as it starts,
    /// cross-fading from the sky before; at warp a turn swings the travel up to its
    /// limit and no further, and turning back moves it at once; after the warp the sky
    /// stays where it was left, and a turn turns it from there.
    #[test]
    fn a_warp_aimed_ahead_travels_ahead_and_turns_only_so_far() {
        use crate::chain::{AIM_FADE_S, WarpAiming};
        let close = |a: [f32; 3], b: [f32; 3]| {
            let (a, b) = (
                crate::warp_aim::from_angles(a),
                crate::warp_aim::from_angles(b),
            );
            a.iter()
                .flatten()
                .zip(b.iter().flatten())
                .all(|(x, y)| (x - y).abs() < 1e-4)
        };
        let aimed = |turn: f32| WarpAiming {
            angles: [turn, 0.0, 0.0],
            ahead: Some(10.0),
        };
        let mut sky = crate::chain::SkyState::default();
        // Still, the sky is as its item turns it.
        sky.step(1, 10.0, (0.0, 1.0), aimed(60.0), (0, 0));
        assert!(sky.aim.is_none() && sky.leaving.is_none());
        assert!(close(sky.angles(aimed(60.0)), [60.0, 0.0, 0.0]));
        // The warp starts: the sky is laid out around straight ahead, fading in from
        // the one before.
        sky.step(2, 10.1, (1.0, 1.0), aimed(60.0), (0, 0));
        assert!(close(sky.angles(aimed(60.0)), [0.0; 3]), "travelling ahead");
        let leaving = sky.leaving.expect("cross-fading");
        assert!(close(leaving.angles, [60.0, 0.0, 0.0]));
        assert!((sky.faded_in(10.1 + AIM_FADE_S / 2.0) - 0.5).abs() < 1e-6);
        // A turn of 5 degrees swings the travel by 5; one of 40 by only 10.
        sky.step(3, 10.2, (1.0, 1.0), aimed(65.0), (0, 0));
        assert!(close(sky.angles(aimed(65.0)), [5.0, 0.0, 0.0]));
        sky.step(4, 10.3, (1.0, 1.0), aimed(100.0), (0, 0));
        assert!(close(sky.angles(aimed(100.0)), [10.0, 0.0, 0.0]));
        // The fade is over.
        sky.step(5, 10.3 + AIM_FADE_S, (1.0, 1.0), aimed(100.0), (0, 0));
        assert!(sky.leaving.is_none());
        // Turning back moves it at once.
        sky.step(6, 11.0, (1.0, 1.0), aimed(95.0), (0, 0));
        assert!(close(sky.angles(aimed(95.0)), [5.0, 0.0, 0.0]));
        // Out of warp, the sky stays and a turn turns it from there.
        sky.step(7, 11.5, (0.0, 0.0), aimed(95.0), (0, 0));
        assert_eq!(sky.warp, 0.0);
        assert!(close(sky.angles(aimed(95.0)), [5.0, 0.0, 0.0]));
        sky.step(8, 12.0, (0.0, 0.0), aimed(125.0), (0, 0));
        assert!(
            close(sky.angles(aimed(125.0)), [35.0, 0.0, 0.0]),
            "no limit"
        );
        // Aimed at the sky again, the item turns it as before.
        sky.step(9, 12.5, (0.0, 0.0), SKY_AIM, (0, 0));
        assert!(sky.aim.is_none());
        // A first frame at warp lays it out at once, with nothing to fade from, however
        // the item is turned and tilted; a pitch past the limit is held to it too.
        let pitched = |tilt: f32| WarpAiming {
            angles: [60.0, tilt, 30.0],
            ahead: Some(10.0),
        };
        let mut sky = crate::chain::SkyState::default();
        sky.step(1, 10.0, (1.0, 1.0), pitched(20.0), (0, 0));
        assert!(close(sky.angles(pitched(20.0)), [0.0; 3]) && sky.leaving.is_none());
        sky.step(2, 10.1, (1.0, 1.0), pitched(50.0), (0, 0));
        let held = crate::warp_aim::from_angles(sky.angles(pitched(50.0)));
        // The cosine of the travel's angle from straight ahead: `-z` turned, read on `-z`.
        let ahead = held[2][2];
        assert!((ahead.acos().to_degrees() - 10.0).abs() < 1e-4, "{ahead}");
    }

    /// ADR-0167: an environment light drawn as a backdrop fills a 3D scene's background
    /// with its sky, under its cards; turned off, or in a scene without a camera, the
    /// background shows; and it stays while a transition runs.
    #[test]
    fn an_environment_backdrop_draws_the_sky_behind_a_3d_scene() {
        let mut graph = placed(&[("cam", (32.0, 18.0), (16.0, 8.0), 0.0)]);
        let main = SceneId::new("main");
        let camera = Edit::SceneSetCamera {
            scene_id: main.clone(),
            camera: Some(jstream_protocol::SceneCamera::framing(
                jstream_protocol::Canvas {
                    width: CANVAS.0,
                    height: CANVAS.1,
                },
            )),
        };
        let blue = [0.0, 0.0, 1.0];
        let sky = |backdrop: bool| jstream_sources::ShownLight {
            kind: jstream_sources::LightType::Environment,
            color: [1.0; 3],
            intensity: 1.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows: false,
            shadow_softness: 0.3,
            environment: Some(jstream_sources::ShownEnvironment {
                sky: blue,
                horizon: blue,
                ground: blue,
                image: None,
                backdrop,
                stars: 0.0,
                star_brightness: 1.0,
                nebula: 0.0,
                nebula_colors: [[0.0; 3]; 2],
                galaxy: 0.0,
                sky_seed: 0,
                warp: 0.0,
                warp_ramp_s: 0.0,
                warp_aim: jstream_sources::WarpAim::Sky,
                warp_look_deg: 10.0,
                sky_pictures: None,
                sky_picture_deg: 30.0,
                sky_picture_light: 1.0,
            }),
        };
        let white = Frame::new(4, 4, &[255; 4 * 4 * 4], 2).expect("invariant: 4x4 RGBA8");
        let draw = |graph: &Graph,
                    light: jstream_sources::ShownLight,
                    running: Option<&crate::transition::Running>| {
            let lamp = Frame::new(1, 1, &[0; 4], 3)
                .expect("invariant: 1x1 RGBA8")
                .with_light(Some(light));
            let shot = snapshot::plan_with(
                graph,
                Some(&main),
                running.map(|running| (running, running.start)),
                &mut |id| {
                    Some(Look::frame(
                        if id.as_str() == "cam" {
                            white.clone()
                        } else {
                            lamp.clone()
                        },
                        None,
                    ))
                },
                &FilterAssets::default(),
            )
            .shot;
            let _gpu = crate::tests::one_gpu_test_at_a_time();
            let mut compositor = gpu()?;
            let mut cache = ResourceCache::new();
            let mut chains = Chains::default();
            Some(draw_points(
                &mut compositor,
                (&mut cache, &mut chains),
                (&shot, &LiveParams::default()),
                &[(2, 2), (32, 18)],
            ))
        };
        // Without a camera there is no backdrop.
        let Some(flat) = draw(&graph, sky(true), None) else {
            return;
        };
        assert_eq!(flat[0][..3], [0, 0, 0], "a 2D scene: {flat:?}");
        apply(&mut graph, vec![camera]);
        let shown = draw(&graph, sky(true), None).expect("an adapter");
        assert_eq!(shown[0], [0, 0, 255, 255], "the sky behind: {shown:?}");
        assert_eq!(
            shown[1],
            [255, 255, 255, 255],
            "the card over it: {shown:?}"
        );
        let off = draw(&graph, sky(false), None).expect("an adapter");
        assert_eq!(off[0][..3], [0, 0, 0], "no backdrop: {off:?}");
        let running = crate::transition::Running {
            from: crate::transition::From::Scene(main.clone()),
            to: main.clone(),
            start: MediaTime::zero(Timebase::MILLIS),
            run: crate::transition::Run {
                preset_id: jstream_protocol::TransitionPresetId::new("move"),
                pairs: true,
                duration_ms: 500,
                easing: jstream_protocol::Easing::Linear,
                unmatched: jstream_protocol::TransitionEffect::None,
                stagger_ms: 0,
                stinger: None,
                orbit: false,
            },
            by_item: true,
            from_camera: crate::transition::camera_of(&graph, &main),
            via: Vec::new(),
            in_place: false,
            carried: Default::default(),
        };
        let switching = draw(&graph, sky(true), Some(&running)).expect("an adapter");
        assert_eq!(
            switching[0],
            [0, 0, 255, 255],
            "while switching: {switching:?}"
        );
        assert_eq!(switching[1], [255, 255, 255, 255], "{switching:?}");
    }

    /// ADR-0188: a backdrop with arrival pictures shows one from its first frame, near
    /// straight ahead; none once the warp is past where the deep sky is gone; and another
    /// as the warp drops out. A warp aimed ahead leaves the one it showed with the sky it
    /// cross-fades from.
    #[test]
    fn a_backdrop_arrives_at_another_picture_after_each_warp() {
        let mut sky = crate::chain::SkyState::default();
        sky.step(1, 10.0, (0.0, 1.0), SKY_AIM, (3, 7));
        let first = sky.arrival.expect("a picture from the start");
        assert!(first.index < 3);
        let ahead = -first.direction[2];
        assert!(ahead > 9.0_f64.to_radians().cos() * 6.0_f64.to_radians().cos() - 1e-9);
        // Into warp: it stays until the deep sky is gone, then none.
        sky.step(2, 10.1, (1.0, 1.0), SKY_AIM, (3, 7));
        assert_eq!(sky.arrival, Some(first), "still there at a little warp");
        sky.step(3, 10.5, (1.0, 1.0), SKY_AIM, (3, 7));
        assert!(sky.arrival.is_none(), "gone at warp {}", sky.warp);
        sky.step(4, 11.5, (1.0, 1.0), SKY_AIM, (3, 7));
        assert!(sky.arrival.is_none(), "none at full warp");
        // Dropping out, frame by frame: none while the deep sky is gone, then exactly
        // one other picture, picked as it starts to show and kept to the end.
        let mut next = None;
        for i in 0..120_u32 {
            sky.step(
                5 + u64::from(i),
                11.5 + f64::from(i + 1) / 60.0,
                (0.0, 1.0),
                SKY_AIM,
                (3, 7),
            );
            let shown = crate::chain::deep_shown(sky.warp, sky.dropping);
            match (next, sky.arrival) {
                (None, None) => assert!(shown <= 0.0, "none though it shows at {}", sky.warp),
                (None, Some(picked)) => {
                    assert!(shown > 0.0);
                    next = Some(picked);
                }
                (Some(kept), now) => assert_eq!(now, Some(kept), "picked again at {}", sky.warp),
            }
        }
        assert_eq!(sky.warp, 0.0, "dropped out");
        let next = next.expect("a new picture as it drops");
        assert_ne!(next.index, first.index);
        // Without pictures, none.
        sky.step(200, 14.0, (0.0, 1.0), SKY_AIM, (0, 7));
        assert!(sky.arrival.is_none());
        // Aimed ahead (ADR-0224), the picture stays where it is seen: the sky it
        // cross-fades from shows it, and the sky laid out anew shows it in the same
        // place, until the deep sky is gone.
        let aimed = crate::chain::WarpAiming {
            angles: [40.0, 10.0, 5.0],
            ahead: Some(10.0),
        };
        let mut sky = crate::chain::SkyState::default();
        sky.step(1, 10.0, (0.0, 1.0), aimed, (2, 7));
        let shown = sky.arrival.expect("a picture");
        let seen = |sky: &crate::chain::SkyState, arrival: crate::chain::Arrival| {
            let r = crate::warp_aim::from_angles(sky.angles(aimed));
            let d = arrival.part([0.0, 0.0]);
            [0, 1, 2].map(|i| (0..3).map(|k| r[i][k] * d[k]).sum::<f64>())
        };
        let before = seen(&sky, shown);
        sky.step(2, 10.1, (1.0, 1.0), aimed, (2, 7));
        assert_eq!(sky.leaving.and_then(|leaving| leaving.arrival), Some(shown));
        let kept = sky.arrival.expect("still shown in the sky laid out ahead");
        assert_eq!(kept.index, shown.index);
        let after = seen(&sky, kept);
        for (a, b) in before.iter().zip(after) {
            assert!((a - b).abs() < 1e-9, "moved: {before:?} to {after:?}");
        }
        sky.step(3, 10.5, (1.0, 1.0), aimed, (2, 7));
        assert!(sky.arrival.is_none(), "gone at warp {}", sky.warp);
        // The shown amount follows the warp as the shader's does.
        assert!((crate::chain::deep_shown(0.0, 0.0) - 1.0).abs() < 1e-6);
        assert!(crate::chain::deep_shown(0.25, 0.0).abs() < 1e-6);
        assert!(crate::chain::deep_shown(0.3, 1.0) > 0.0);
    }

    /// ADR-0224: a warp setting that follows a channel or a sequence down out of warp,
    /// with no ramp of its own, so it is never above its setting, picks the next
    /// picture as the deep sky starts to show again, not only once the warp is still,
    /// so it fades in with the deep sky rather than popping in at rest.
    #[test]
    fn an_arrival_picture_is_gone_as_the_stars_start_to_move() {
        use crate::chain::{deep_shown, picture_opacity, picture_shown};
        // Going in (ADR-0239): full at rest, gone by the first touch of warp, while the
        // deep sky still shows.
        assert!((picture_shown(0.0, 0.0) - 1.0).abs() < 1e-6);
        assert!(picture_shown(0.03, 0.0) < 0.5);
        assert_eq!(picture_shown(0.05, 0.0), 0.0);
        assert_eq!(picture_shown(0.33, 0.0), 0.0);
        assert!(deep_shown(0.05, 0.0) > 0.85);
        // Dropping out, it comes back with the deep sky (ADR-0188 decision 5).
        for warp in [0.1_f32, 0.2, 0.3, 0.4, 0.5] {
            assert!((picture_shown(warp, 1.0) - deep_shown(warp, 1.0)).abs() < 1e-6);
        }
        // Never more than the deep sky, and its opacity over the deep sky's fade gives
        // it in all.
        for i in 0..=100_u8 {
            let warp = f32::from(i) / 100.0;
            for dropping in [0.0_f32, 0.3, 1.0] {
                let (picture, deep) = (picture_shown(warp, dropping), deep_shown(warp, dropping));
                assert!(picture <= deep + 1e-6, "{warp} {dropping}");
                let opacity = picture_opacity(warp, dropping);
                assert!((0.0..=1.0).contains(&opacity));
                assert!((opacity * deep - picture).abs() < 1e-5, "{warp} {dropping}");
            }
        }
    }

    #[test]
    fn a_warp_followed_down_picks_its_arrival_as_the_sky_shows() {
        let mut sky = crate::chain::SkyState::default();
        sky.step(1, 10.0, (0.0, 0.0), SKY_AIM, (3, 7));
        let first = sky.arrival.expect("a picture from the start");
        // Up into warp, frame by frame, as a sequence takes it there.
        for i in 0..60_u32 {
            let warp = (f64::from(i + 1) / 60.0) as f32;
            sky.step(
                2 + u64::from(i),
                10.0 + f64::from(i + 1) / 60.0,
                (warp, 0.0),
                SKY_AIM,
                (3, 7),
            );
            assert!(
                sky.arrival.is_none() || sky.arrival == Some(first),
                "picked on the way in at {warp}"
            );
        }
        assert!(sky.arrival.is_none(), "none at full warp");
        // Down out of it, as a sequence takes it: exactly one other picture, picked
        // while the warp is still falling and fading in.
        let mut picked_at = None;
        for i in 0..60_u32 {
            let warp = 1.0 - (f64::from(i + 1) / 60.0) as f32;
            sky.step(
                100 + u64::from(i),
                12.0 + f64::from(i + 1) / 60.0,
                (warp, 0.0),
                SKY_AIM,
                (3, 7),
            );
            assert_eq!(sky.dropping, 0.0, "followed, never above its setting");
            if let (None, Some(next)) = (picked_at, sky.arrival) {
                assert_ne!(next.index, first.index);
                picked_at = Some(sky.warp);
            }
        }
        let at = picked_at.expect("a picture as it drops");
        assert!(at > 0.0, "picked only at rest");
        assert!(
            crate::chain::deep_shown(at, 0.0) < 0.5,
            "picked faint, at {at}"
        );
    }

    /// ADR-0188: an environment backdrop shows its arrival picture ahead, over the sky,
    /// and lights the scene from there in the picture's colour.
    #[test]
    fn an_arrival_picture_shows_ahead_and_lights_the_scene() {
        arrives_at(false);
    }

    /// ADR-0198: an arrival of several pictures, a nested scene's, shows its first ahead
    /// and the others turned from it, and lights the scene from its last: a sun turned
    /// a quarter to the right of a planet lights it from the right in the sun's colour.
    #[test]
    fn an_arrival_of_a_planet_and_its_sun_lights_from_the_sun() {
        arrives_at(true);
    }

    /// The arrival test, with a planet alone or a planet and a sun a quarter to its right
    /// (`system`).
    fn arrives_at(system: bool) {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut graph = placed(&[("cam", (4.0, 4.0), (2.0, 2.0), 0.0)]);
        let main = SceneId::new("main");
        let arrivals = SceneId::new("arrivals");
        apply(
            &mut graph,
            vec![
                Edit::SceneSetCamera {
                    scene_id: main.clone(),
                    camera: Some(jstream_protocol::SceneCamera::framing(
                        jstream_protocol::Canvas {
                            width: CANVAS.0,
                            height: CANVAS.1,
                        },
                    )),
                },
                Edit::SceneCreate {
                    scene_id: arrivals.clone(),
                    name: "Arrivals".into(),
                    role: None,
                },
                Edit::SourceCreate {
                    source_id: SourceId::new("planet"),
                    name: "planet".into(),
                    kind: SourceKind::Builtin {
                        kind: "display".into(),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
            ],
        );
        let solar = SceneId::new("solar");
        let mut sun_at = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(960.0, 540.0).expect("finite"),
            jstream_protocol::Size::try_new(1920.0, 1080.0).expect("positive"),
        )
        .expect("valid");
        sun_at.turn_deg = jstream_protocol::Finite32::new(90.0).expect("finite");
        let add = |scene: &SceneId, item: &str, source: &str, geometry| Edit::ItemAdd {
            scene_id: scene.clone(),
            item_id: ItemId::new(item),
            source_id: SourceId::new(source),
            transform: None,
            geometry,
            opacity: None,
            blend: None,
        };
        apply(
            &mut graph,
            if system {
                vec![
                    Edit::SceneCreate {
                        scene_id: solar.clone(),
                        name: "Solar".into(),
                        role: None,
                    },
                    Edit::SourceCreate {
                        source_id: SourceId::new("sun"),
                        name: "sun".into(),
                        kind: SourceKind::Builtin {
                            kind: "display".into(),
                        },
                        settings: Default::default(),
                        duplicated_from: None,
                    },
                    Edit::SourceCreate {
                        source_id: SourceId::new("solar"),
                        name: "solar".into(),
                        kind: SourceKind::Scene {
                            scene_id: solar.clone(),
                        },
                        settings: Default::default(),
                        duplicated_from: None,
                    },
                    add(&solar, "planet", "planet", None),
                    add(&solar, "sun", "sun", Some(sun_at)),
                    add(&arrivals, "solar", "solar", None),
                ]
            } else {
                vec![add(&arrivals, "planet", "planet", None)]
            },
        );
        let sky = jstream_sources::ShownLight {
            kind: jstream_sources::LightType::Environment,
            color: [1.0; 3],
            intensity: 1.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows: false,
            shadow_softness: 0.3,
            environment: Some(jstream_sources::ShownEnvironment {
                sky: [0.0; 3],
                horizon: [0.0; 3],
                ground: [0.0; 3],
                image: None,
                backdrop: true,
                stars: 0.0,
                star_brightness: 1.0,
                nebula: 0.0,
                nebula_colors: [[0.0; 3]; 2],
                galaxy: 0.0,
                sky_seed: 0,
                warp: 0.0,
                warp_ramp_s: 1.0,
                warp_aim: jstream_sources::WarpAim::Sky,
                warp_look_deg: 10.0,
                sky_pictures: Some(arrivals.clone()),
                sky_picture_deg: 60.0,
                sky_picture_light: 2.0,
            }),
        };
        let white = Frame::new(4, 4, &[255; 4 * 4 * 4], 2).expect("invariant: 4x4 RGBA8");
        let red = Frame::new(4, 4, &[255, 0, 0, 255].repeat(16), 4).expect("invariant: 4x4 RGBA8");
        let green =
            Frame::new(4, 4, &[0, 255, 0, 255].repeat(16), 5).expect("invariant: 4x4 RGBA8");
        let lamp = Frame::new(1, 1, &[0; 4], 3)
            .expect("invariant: 1x1 RGBA8")
            .with_light(Some(sky));
        let shot = snapshot::plan_with(
            &graph,
            Some(&main),
            None,
            &mut |id| {
                Some(Look::frame(
                    match id.as_str() {
                        "cam" => white.clone(),
                        "planet" => red.clone(),
                        "sun" => green.clone(),
                        _ => lamp.clone(),
                    },
                    None,
                ))
            },
            &FilterAssets::default(),
        )
        .shot;
        let planned = shot
            .sky_pictures
            .get(&SourceId::new("logo"))
            .expect("the light's arrival pictures are planned");
        assert_eq!(planned.len(), 1, "one arrival");
        let parts = &planned[0];
        assert_eq!(parts[0].source, SourceId::new("planet"));
        assert_eq!(parts[0].offset_deg, [0.0; 2]);
        assert!(
            (parts[0].scale - 1.0).abs() < 1e-6,
            "a full-canvas item: its setting"
        );
        if system {
            assert_eq!(parts.len(), 2);
            assert_eq!(parts[1].source, SourceId::new("sun"));
            assert_eq!(parts[1].offset_deg, [90.0, 0.0]);
        } else {
            assert_eq!(parts.len(), 1);
        }
        // A picture's item scales its width: a box half the canvas wide, half.
        let canvas = jstream_protocol::Canvas::DEFAULT;
        let half = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(0.0, 0.0).expect("finite"),
            jstream_protocol::Size::try_new(canvas.width as f32 / 2.0, 10.0).expect("positive"),
        )
        .expect("valid");
        let scale = snapshot::picture_scale(&jstream_graph::Placement::Geometry(half), canvas);
        assert!((scale - 0.5).abs() < 1e-6, "{scale}");
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let centre = draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (&shot, &LiveParams::default()),
            &[(CANVAS.0 / 2, CANVAS.1 / 2)],
        )[0];
        assert!(
            centre[0] > 200 && centre[1] < 30 && centre[2] < 30,
            "the picture ahead: {centre:?}"
        );
        // Its colour is taken from the frame after it is picked: only the shown one's is.
        assert!(
            chains.picture_means.is_empty(),
            "none averaged before a pick"
        );
        draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (&shot, &LiveParams::default()),
            &[(CANVAS.0 / 2, CANVAS.1 / 2)],
        );
        assert_eq!(chains.picture_means.len(), 1, "only the shown one");
        // A draw of a mix that shows no arrival (another scene) keeps it: dropped, the
        // program's next draw asked the let-go still for its pixels again and decoded
        // it on every frame, its light white meanwhile.
        let other = snapshot::plan_with(
            &graph,
            Some(&arrivals),
            None,
            &mut |_| Some(Look::frame(white.clone(), None)),
            &FilterAssets::default(),
        )
        .shot;
        draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (&other, &LiveParams::default()),
            &[(CANVAS.0 / 2, CANVAS.1 / 2)],
        );
        assert_eq!(
            chains.picture_means.len(),
            1,
            "kept while another mix draws"
        );
        let light = snapshot::arrival_light(
            (&shot, &cache, &chains),
            &main,
            MediaTime::zero(Timebase::MILLIS),
            &LiveParams::default(),
        )
        .expect("it lights the scene");
        assert_eq!(light.kind, jstream_render::LightKind::Directional);
        assert!((light.intensity - 2.0).abs() < 1e-6);
        if system {
            // From the sun, a quarter to the right, toward the left, in its colour.
            assert!(
                light.color.g > 0.99 && light.color.r < 0.01,
                "{:?}",
                light.color
            );
            assert!(light.aim[0] < -0.9, "{:?}", light.aim);
        } else {
            assert!(
                light.color.r > 0.99 && light.color.g < 0.01,
                "{:?}",
                light.color
            );
            // It shines from ahead, back toward the viewer.
            assert!(light.aim[2] > 0.9, "{:?}", light.aim);
        }
        // ADR-0238: a keyframe of the scene, another view of it, shows the same sky:
        // one sky state for the family, the arrival it picked unchanged, and the
        // picture lighting the view too.
        let arrived = chains.skies.borrow().get(&main).and_then(|sky| sky.arrival);
        assert!(arrived.is_some(), "the scene's sky picked an arrival");
        let view = SceneId::new("view");
        apply(
            &mut graph,
            vec![Edit::SceneCreateVariant {
                scene_id: view.clone(),
                variant_of: main.clone(),
                name: "View".into(),
            }],
        );
        let viewed = snapshot::plan_with(
            &graph,
            Some(&view),
            None,
            &mut |id| {
                Some(Look::frame(
                    match id.as_str() {
                        "cam" => white.clone(),
                        "planet" => red.clone(),
                        "sun" => green.clone(),
                        _ => lamp.clone(),
                    },
                    None,
                ))
            },
            &FilterAssets::default(),
        )
        .shot;
        draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (&viewed, &LiveParams::default()),
            &[(CANVAS.0 / 2, CANVAS.1 / 2)],
        );
        assert_eq!(
            chains.skies.borrow().keys().cloned().collect::<Vec<_>>(),
            vec![main.clone()],
            "one sky for the scene and its view"
        );
        assert_eq!(
            chains.skies.borrow().get(&main).and_then(|sky| sky.arrival),
            arrived,
            "the view shows the arrival the scene picked"
        );
        assert!(
            snapshot::arrival_light(
                (&viewed, &cache, &chains),
                &view,
                MediaTime::zero(Timebase::MILLIS),
                &LiveParams::default(),
            )
            .is_some(),
            "and it lights the view"
        );
    }

    /// ADR-0187: as a warp aimed ahead starts, the backdrop cross-fades from the sky its
    /// item turned to one laid out around straight ahead: an item tilted to show the
    /// sky's zenith ahead shows its horizon ahead once the fade is over, and a mix of
    /// the two halfway.
    #[test]
    fn a_warp_aimed_ahead_cross_fades_the_backdrop_to_straight_ahead() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut graph = placed(&[("cam", (32.0, 18.0), (4.0, 4.0), 0.0)]);
        let main = SceneId::new("main");
        let mut tilted = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(4.0, 4.0).expect("positive"),
        )
        .expect("valid");
        tilted.tilt_deg = jstream_protocol::Finite32::new(90.0).expect("finite");
        apply(
            &mut graph,
            vec![
                Edit::SceneSetCamera {
                    scene_id: main.clone(),
                    camera: Some(jstream_protocol::SceneCamera::framing(
                        jstream_protocol::Canvas {
                            width: CANVAS.0,
                            height: CANVAS.1,
                        },
                    )),
                },
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("logo"),
                    geometry: tilted,
                },
            ],
        );
        let sky = |warp: f32| jstream_sources::ShownLight {
            kind: jstream_sources::LightType::Environment,
            color: [1.0; 3],
            intensity: 1.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows: false,
            shadow_softness: 0.3,
            environment: Some(jstream_sources::ShownEnvironment {
                sky: [1.0, 0.0, 0.0],
                horizon: [0.0, 1.0, 0.0],
                ground: [0.0, 0.0, 1.0],
                image: None,
                backdrop: true,
                stars: 0.0,
                star_brightness: 1.0,
                nebula: 0.0,
                nebula_colors: [[0.0; 3]; 2],
                galaxy: 0.0,
                sky_seed: 0,
                warp,
                // So slow the warp's glow stays faint over the test.
                warp_ramp_s: 10.0,
                warp_aim: jstream_sources::WarpAim::Ahead,
                warp_look_deg: 10.0,
                sky_pictures: None,
                sky_picture_deg: 30.0,
                sky_picture_light: 1.0,
            }),
        };
        let white = Frame::new(4, 4, &[255; 4 * 4 * 4], 2).expect("invariant: 4x4 RGBA8");
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let mut draw = |warp: f32, ms: i64| {
            let lamp = Frame::new(1, 1, &[0; 4], 3)
                .expect("invariant: 1x1 RGBA8")
                .with_light(Some(sky(warp)));
            let shot = snapshot::plan_with(
                &graph,
                Some(&main),
                None,
                &mut |id| {
                    Some(Look::frame(
                        if id.as_str() == "cam" {
                            white.clone()
                        } else {
                            lamp.clone()
                        },
                        None,
                    ))
                },
                &FilterAssets::default(),
            )
            .shot;
            cache.begin_frame();
            let canvas = shot.canvas().expect("canvas");
            let output = snapshot::compose(
                &mut compositor,
                (&mut cache, &mut chains),
                (&shot, MediaTime::new(ms, Timebase::MILLIS)),
                snapshot::View::default(),
                &LiveParams::default(),
                &NoCaptures,
                snapshot::DrawAt::read(canvas),
            )
            .expect("compose");
            // Just beside the card, straight ahead.
            let pixel = output
                .pixels()
                .expect("read back")
                .pixel(36, 18)
                .expect("a pixel");
            drop(cache.end_frame(&compositor));
            pixel
        };
        let still = draw(0.0, 0);
        assert!(
            still[0] > 200 && still[1] < 40,
            "the zenith ahead: {still:?}"
        );
        let starting = draw(1.0, 100);
        assert!(
            starting[0] > 200 && starting[1] < 40,
            "the fade starts from it: {starting:?}"
        );
        let halfway = draw(1.0, 100 + (crate::chain::AIM_FADE_S * 500.0) as i64);
        assert!(
            halfway[0] > 60 && halfway[1] > 60,
            "a mix halfway: {halfway:?}"
        );
        let over = draw(1.0, 100 + (crate::chain::AIM_FADE_S * 1000.0) as i64 + 50);
        assert!(over[1] > 200 && over[0] < 40, "the horizon ahead: {over:?}");
    }

    /// ADR-0143 decisions 2 and 3: a `scene`-lit model in a 3D scene is lit by the light
    /// items placed there, where their items put them and as bright as their opacity
    /// lets them be; with none it is dark, and in a 2D scene it is studio-lit.
    #[test]
    fn a_scene_lit_model_is_lit_by_the_lights_its_scene_places() {
        let mut graph = placed(&[("cam", (32.0, 18.0), (32.0, 36.0), 0.0)]);
        let lamp = |depth: f32| {
            let mut geometry = jstream_protocol::ItemGeometry::try_new(
                jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
                jstream_protocol::Size::try_new(8.0, 8.0).expect("positive"),
            )
            .expect("valid");
            geometry.depth_px = jstream_protocol::Finite32::new(depth).expect("finite");
            Edit::ItemSetGeometry {
                scene_id: None,
                item_id: ItemId::new("logo"),
                geometry,
            }
        };
        let light = |kind| jstream_sources::ShownLight {
            kind,
            color: [1.0; 3],
            intensity: 2.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows: false,
            shadow_softness: 0.3,
            environment: None,
        };
        let point = light(jstream_sources::LightType::Point);
        let luma = |pixel: [u8; 4]| pixel[..3].iter().map(|&c| u32::from(c)).sum::<u32>();
        // In a 2D scene the plane is studio-lit, whatever lights are placed.
        apply(&mut graph, vec![lamp(-200.0)]);
        let Some(flat) = lit_centre(&graph, point.clone()) else {
            return;
        };
        assert!(luma(flat) > 60, "studio-lit in a 2D scene: {flat:?}");
        apply(
            &mut graph,
            vec![Edit::SceneSetCamera {
                scene_id: SceneId::new("main"),
                camera: Some(jstream_protocol::SceneCamera::framing(
                    jstream_protocol::Canvas {
                        width: CANVAS.0,
                        height: CANVAS.1,
                    },
                )),
            }],
        );
        // Behind the plane, a point light lights only its back, which isn't drawn.
        let behind = lit_centre(&graph, point.clone()).expect("an adapter");
        assert!(luma(behind) < 6, "lit from behind: {behind:?}");
        // In front of it, the same light lights its face.
        apply(&mut graph, vec![lamp(200.0)]);
        let front = lit_centre(&graph, point.clone()).expect("an adapter");
        assert!(luma(front) > 150, "lit from the front: {front:?}");
        // The item's opacity scales the light: at 0 it is off.
        apply(
            &mut graph,
            vec![Edit::ItemSetOpacity {
                item_id: ItemId::new("logo"),
                scene_id: None,
                opacity: jstream_protocol::Finite32::new(0.0).expect("finite"),
            }],
        );
        let off = lit_centre(&graph, point.clone()).expect("an adapter");
        assert!(luma(off) < 6, "a light at opacity 0 is off: {off:?}");
        // An ambient light lights it from everywhere, wherever its item is.
        apply(
            &mut graph,
            vec![
                Edit::ItemSetOpacity {
                    item_id: ItemId::new("logo"),
                    scene_id: None,
                    opacity: jstream_protocol::Finite32::new(1.0).expect("finite"),
                },
                lamp(-200.0),
            ],
        );
        let ambient =
            lit_centre(&graph, light(jstream_sources::LightType::Ambient)).expect("an adapter");
        assert!(luma(ambient) > 30, "ambient light: {ambient:?}");
    }

    /// ADR-0171 decision 4: a light item's `source` modifiers change how it lights the
    /// scene, and a model item's change how it draws its materials, each frame, with
    /// nothing stored.
    #[test]
    fn source_modifiers_drive_a_lights_and_a_models_settings() {
        let mut graph = placed(&[("cam", (32.0, 18.0), (32.0, 36.0), 0.0)]);
        let mut geometry = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(8.0, 8.0).expect("positive"),
        )
        .expect("valid");
        geometry.depth_px = jstream_protocol::Finite32::new(200.0).expect("finite");
        apply(
            &mut graph,
            vec![
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("logo"),
                    geometry,
                },
                Edit::SceneSetCamera {
                    scene_id: SceneId::new("main"),
                    camera: Some(jstream_protocol::SceneCamera::framing(
                        jstream_protocol::Canvas {
                            width: CANVAS.0,
                            height: CANVAS.1,
                        },
                    )),
                },
            ],
        );
        let point = jstream_sources::ShownLight {
            kind: jstream_sources::LightType::Point,
            color: [1.0; 3],
            intensity: 2.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows: false,
            shadow_softness: 0.3,
            environment: None,
        };
        let luma = |pixel: [u8; 4]| pixel[..3].iter().map(|&c| u32::from(c)).sum::<u32>();
        let Some(lit) = lit_centre(&graph, point.clone()) else {
            return;
        };
        assert!(luma(lit) > 150, "lit from the front: {lit:?}");
        // A square wave starts at its amplitude: these hold their value at the start.
        let held = |item: &str, param: &str, value: f32| Edit::ItemSetMotion {
            item_id: ItemId::new(item),
            scene_id: None,
            motion: vec![jstream_protocol::MotionModifier {
                mode: jstream_protocol::MappingMode::Add,
                modifier_id: "drive".into(),
                property: jstream_protocol::MotionProperty::Source,
                effect: None,
                source: Some(jstream_protocol::MotionSetting {
                    param: param.into(),
                    material: None,
                }),
                kind: jstream_protocol::MotionKind::Oscillate {
                    shape: jstream_protocol::MotionShape::Square,
                    amplitude: jstream_protocol::Finite32::new(value.abs()).expect("finite"),
                    period_ms: 3_600_000,
                    phase: jstream_protocol::Finite32::new(if value < 0.0 { 0.5 } else { 0.0 })
                        .expect("finite"),
                },
                clock: jstream_protocol::MotionClock::Engine,
                enabled: true,
                amount: None,
            }],
        };
        // The light's intensity driven to 0 puts it out; its colour to black, too.
        apply(&mut graph, vec![held("logo", "intensity", -2.0)]);
        let out = lit_centre(&graph, point.clone()).expect("an adapter");
        assert!(luma(out) < 6, "intensity driven to 0: {out:?}");
        apply(&mut graph, vec![held("logo", "color", -1.0)]);
        let black = lit_centre(&graph, point.clone()).expect("an adapter");
        assert!(luma(black) < 6, "colour driven to black: {black:?}");
        // The model's base colour driven to black darkens it under the same light.
        apply(
            &mut graph,
            vec![
                Edit::ItemSetMotion {
                    item_id: ItemId::new("logo"),
                    scene_id: None,
                    motion: Vec::new(),
                },
                held("cam", "color", -1.0),
            ],
        );
        let dark = lit_centre(&graph, point).expect("an adapter");
        // Black base colour leaves only the specular highlight.
        assert!(
            luma(dark) < luma(lit) / 2,
            "model driven dark: {dark:?} from {lit:?}"
        );
    }

    /// Points of `graph`'s program drawn once: `cam` shows a plane model when `model`
    /// is set, and every other source a white frame. `None` where the machine has no
    /// adapter.
    fn drawn_points(graph: &Graph, model: bool, points: &[(u32, u32)]) -> Option<Vec<[u8; 4]>> {
        use jstream_sources::SourceKind as _;
        let white: Vec<u8> = std::iter::repeat_n(WHITE, 16 * 9).flatten().collect();
        let plain = Frame::new(16, 9, &white, 1).expect("invariant: 16x9 RGBA8");
        let kind = jstream_sources::ModelKind::new();
        let mut settings: jstream_protocol::Settings = kind
            .descriptor()
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect();
        settings.insert(String::from("shape"), serde_json::json!("plane"));
        settings.insert(String::from("color"), serde_json::json!("#ffffff"));
        // Unlit, so it is white and no brighter: studio light can take a layer past 1.
        settings.insert(String::from("lighting"), serde_json::json!("unlit"));
        let plane = kind
            .create(&settings, &jstream_sources::Context::default())
            .expect("a model");
        let modelled = plain.clone().with_model(plane.model());
        let shot = snapshot::plan_with(
            graph,
            Some(&SceneId::new("main")),
            None,
            &mut |id| {
                Some(Look::frame(
                    if model && id.as_str() == "cam" {
                        modelled.clone()
                    } else {
                        plain.clone()
                    },
                    None,
                ))
            },
            &FilterAssets::default(),
        )
        .shot;
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let mut compositor = gpu()?;
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        Some(draw_points(
            &mut compositor,
            (&mut cache, &mut chains),
            (&shot, &LiveParams::default()),
            points,
        ))
    }

    /// ADR-0143 decisions 4 to 6: `depth_fog` on a 3D scene's chain fogs each card and
    /// model by its own distance from the camera, read from the view depth the chains
    /// draw for it; on a 2D scene it has no depth and passes the picture through.
    #[test]
    fn depth_fog_on_a_scene_fogs_cards_and_models_by_their_distance() {
        let canvas = jstream_protocol::Canvas {
            width: CANVAS.0,
            height: CANVAS.1,
        };
        let camera = jstream_protocol::SceneCamera::framing(canvas);
        let f = camera.view(canvas).expect("a view").focal_px();
        // `logo` on the canvas plane at the left; `cam` a focal length behind it at
        // the right, seen half as large about the centre, so its centre is at x = 40.
        let mut graph = placed(&[
            ("logo", (16.0, 18.0), (32.0, 36.0), 0.0),
            ("cam", (48.0, 18.0), (32.0, 36.0), 0.0),
        ]);
        let mut far = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(48.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(32.0, 36.0).expect("positive"),
        )
        .expect("valid");
        far.fit_mode = jstream_protocol::FitMode::Stretch;
        far.depth_px = jstream_protocol::Finite32::new(-f).expect("finite");
        // Black fog from one focal length to three, and none where nothing is: the
        // near card is clear, and anything at twice the focal length half fogged.
        let fog = Edit::FilterAdd {
            owner: FilterOwner::Scene {
                scene_id: SceneId::new("main"),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new("fog"),
            kind: "depth_fog".into(),
            settings: [
                ("mode", serde_json::json!("linear")),
                ("color", serde_json::json!("#000000ff")),
                ("start_px", serde_json::json!(f)),
                ("end_px", serde_json::json!(3.0 * f)),
                ("density", serde_json::json!(0.5)),
                ("max", serde_json::json!(1.0)),
                ("background", serde_json::json!(false)),
            ]
            .into_iter()
            .collect(),
            enabled: Some(true),
            index: None,
        };
        apply(
            &mut graph,
            vec![
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("cam"),
                    geometry: far,
                },
                fog,
            ],
        );
        let luma = |pixel: [u8; 4]| pixel[..3].iter().map(|&c| u32::from(c)).sum::<u32>();
        let points = [(12, 18), (40, 18)];
        // In 2D the fog has no depth: the picture passes through.
        let Some(flat) = drawn_points(&graph, false, &points) else {
            return;
        };
        assert!(
            flat.iter().all(|&pixel| luma(pixel) > 700),
            "a 2D scene isn't fogged: {flat:?}"
        );
        apply(
            &mut graph,
            vec![Edit::SceneSetCamera {
                scene_id: SceneId::new("main"),
                camera: Some(camera),
            }],
        );
        let mut clear = graph.clone();
        apply(
            &mut clear,
            vec![Edit::FilterSetEnabled {
                owner: FilterOwner::Scene {
                    scene_id: SceneId::new("main"),
                },
                filter_id: FilterId::new("fog"),
                enabled: false,
            }],
        );
        // A model whose item has effects is placed through their output, and still
        // writes its own depth.
        let mut affected = graph.clone();
        apply(&mut affected, vec![item_tint("cam", "#ffffff")]);
        let mut affected_clear = clear.clone();
        apply(&mut affected_clear, vec![item_tint("cam", "#ffffff")]);
        for (model, what, clear, graph) in [
            (false, "card", &clear, &graph),
            (true, "model", &clear, &graph),
            (true, "model with effects", &affected_clear, &affected),
        ] {
            let seen = drawn_points(clear, model, &points).expect("an adapter");
            let fogged = drawn_points(graph, model, &points).expect("an adapter");
            assert!(
                luma(fogged[0]) + 6 >= luma(seen[0]),
                "the near card is clear: {:?} against {:?}",
                fogged[0],
                seen[0]
            );
            // Half fogged at twice the focal length: linear 0.5 of white, sRGB 188.
            assert_eq!(seen[1], WHITE, "the far {what} unfogged");
            assert!(
                fogged[1][..3].iter().all(|&c| c.abs_diff(188) <= 3),
                "the far {what} is half fogged: {:?}",
                fogged[1]
            );
        }
    }

    /// ADR-0151 decisions 1 and 6: a 3D scene's depth-aware filters get its camera and
    /// lights with its view depth, so black volumetric fog on its chain glows where a
    /// light item of the scene reaches it, and stays black without one; a 2D scene
    /// passes the picture through.
    #[test]
    fn volumetric_fog_on_a_scene_is_lit_by_its_lights() {
        let canvas = jstream_protocol::Canvas {
            width: CANVAS.0,
            height: CANVAS.1,
        };
        let mut graph = placed(&[("cam", (32.0, 18.0), (64.0, 36.0), 0.0)]);
        let mut lamp = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(8.0, 8.0).expect("positive"),
        )
        .expect("valid");
        lamp.depth_px = jstream_protocol::Finite32::new(20.0).expect("finite");
        let fog = Edit::FilterAdd {
            owner: FilterOwner::Scene {
                scene_id: SceneId::new("main"),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new("fog"),
            kind: "volumetric_fog".into(),
            settings: [
                ("color", serde_json::json!("#000000ff")),
                ("density", serde_json::json!(5.0)),
                ("falloff_px", serde_json::json!(0.0)),
                ("noise", serde_json::json!(0.0)),
                ("scattering", serde_json::json!(1.0)),
            ]
            .into_iter()
            .collect(),
            enabled: Some(true),
            index: None,
        };
        apply(
            &mut graph,
            vec![
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("logo"),
                    geometry: lamp,
                },
                fog,
            ],
        );
        let light = jstream_sources::ShownLight {
            kind: jstream_sources::LightType::Point,
            color: [1.0; 3],
            intensity: 4.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows: false,
            shadow_softness: 0.3,
            environment: None,
        };
        let black =
            Frame::new(16, 9, &[0, 0, 0, 255].repeat(16 * 9), 2).expect("invariant: 16x9 RGBA8");
        let lamp = Frame::new(1, 1, &[0; 4], 3)
            .expect("invariant: 1x1 RGBA8")
            .with_light(Some(light));
        let centre = |graph: &Graph| {
            let shot = snapshot::plan_with(
                graph,
                Some(&SceneId::new("main")),
                None,
                &mut |id| {
                    Some(Look::frame(
                        if id.as_str() == "cam" {
                            black.clone()
                        } else {
                            lamp.clone()
                        },
                        None,
                    ))
                },
                &FilterAssets::default(),
            )
            .shot;
            let _gpu = crate::tests::one_gpu_test_at_a_time();
            let mut compositor = gpu()?;
            let mut cache = ResourceCache::new();
            let mut chains = Chains::default();
            Some(
                draw_points(
                    &mut compositor,
                    (&mut cache, &mut chains),
                    (&shot, &LiveParams::default()),
                    &[(32, 18)],
                )[0],
            )
        };
        let luma = |pixel: [u8; 4]| pixel[..3].iter().map(|&c| u32::from(c)).sum::<u32>();
        let Some(flat) = centre(&graph) else {
            return;
        };
        assert!(luma(flat) < 6, "a 2D scene isn't fogged: {flat:?}");
        apply(
            &mut graph,
            vec![Edit::SceneSetCamera {
                scene_id: SceneId::new("main"),
                camera: Some(jstream_protocol::SceneCamera::framing(canvas)),
            }],
        );
        let lit = centre(&graph).expect("an adapter");
        assert!(luma(lit) > 60, "the light glows in the fog: {lit:?}");
        apply(&mut graph, vec![hide("logo")]);
        let dark = centre(&graph).expect("an adapter");
        assert!(luma(dark) < 6, "no light, no glow: {dark:?}");
    }

    /// ADR-0151 decision 4: a scene whose chain holds a floor reflection hands it the
    /// scene drawn mirrored in the filter's floor, with that picture's depth; a 2D scene
    /// hands it none.
    #[test]
    fn a_floor_reflection_gets_its_scene_mirrored_in_its_floor() {
        let canvas = jstream_protocol::Canvas {
            width: CANVAS.0,
            height: CANVAS.1,
        };
        let mut graph = placed(&[("cam", (32.0, 18.0), (64.0, 36.0), 0.0)]);
        let mirror = Edit::FilterAdd {
            owner: FilterOwner::Scene {
                scene_id: SceneId::new("main"),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new("mirror"),
            kind: "floor_reflection".into(),
            settings: [("floor_y_px", serde_json::json!(30.0))]
                .into_iter()
                .collect(),
            enabled: Some(true),
            index: None,
        };
        apply(&mut graph, vec![mirror]);
        let white = Frame::new(16, 9, &[255; 4].repeat(16 * 9), 2).expect("invariant: 16x9 RGBA8");
        let mirrored = |graph: &Graph| {
            let shot = snapshot::plan_with(
                graph,
                Some(&SceneId::new("main")),
                None,
                &mut |_| Some(Look::frame(white.clone(), None)),
                &FilterAssets::default(),
            )
            .shot;
            let _gpu = crate::tests::one_gpu_test_at_a_time();
            let mut compositor = gpu()?;
            let mut cache = ResourceCache::new();
            let mut chains = Chains::default();
            draw_points(
                &mut compositor,
                (&mut cache, &mut chains),
                (&shot, &LiveParams::default()),
                &[(32, 18)],
            );
            Some(
                chains
                    .depths
                    .get(&SceneId::new("main"))
                    .and_then(|(_, depth)| depth.reflection.as_ref())
                    .map(|r| (r.floor_y, r.image.width(), r.depth.height())),
            )
        };
        let Some(flat) = mirrored(&graph) else {
            return;
        };
        assert_eq!(flat, None, "a 2D scene draws no reflection");
        apply(
            &mut graph,
            vec![Edit::SceneSetCamera {
                scene_id: SceneId::new("main"),
                camera: Some(jstream_protocol::SceneCamera::framing(canvas)),
            }],
        );
        assert_eq!(
            mirrored(&graph).expect("an adapter"),
            Some((30.0, CANVAS.0, CANVAS.1)),
            "the scene mirrored in the floor reaches the filter"
        );
    }

    /// ADR-0151 decision 10: a `lit` card in a 3D scene is shaded by the scene's lights,
    /// so a red light turns a white card red; unlit, or with no light, it draws as it
    /// is.
    #[test]
    fn a_lit_card_is_shaded_by_its_scenes_lights() {
        let canvas = jstream_protocol::Canvas {
            width: CANVAS.0,
            height: CANVAS.1,
        };
        let mut graph = placed(&[("cam", (32.0, 18.0), (64.0, 36.0), 0.0)]);
        let mut lamp = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(8.0, 8.0).expect("positive"),
        )
        .expect("valid");
        lamp.depth_px = jstream_protocol::Finite32::new(40.0).expect("finite");
        let mut card = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(64.0, 36.0).expect("positive"),
        )
        .expect("valid");
        card.fit_mode = jstream_protocol::FitMode::Stretch;
        let lit = |on: bool| Edit::ItemSetGeometry {
            scene_id: None,
            item_id: ItemId::new("cam"),
            geometry: jstream_protocol::ItemGeometry {
                surface: Some(jstream_protocol::Surface {
                    lit: on,
                    ..jstream_protocol::Surface::DEFAULT
                }),
                ..card
            },
        };
        apply(
            &mut graph,
            vec![
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("logo"),
                    geometry: lamp,
                },
                Edit::SceneSetCamera {
                    scene_id: SceneId::new("main"),
                    camera: Some(jstream_protocol::SceneCamera::framing(canvas)),
                },
                lit(false),
            ],
        );
        let light = jstream_sources::ShownLight {
            kind: jstream_sources::LightType::Point,
            color: [1.0, 0.0, 0.0],
            intensity: 2.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows: false,
            shadow_softness: 0.3,
            environment: None,
        };
        let white = Frame::new(16, 9, &[255; 4].repeat(16 * 9), 2).expect("invariant: 16x9 RGBA8");
        let lamp = Frame::new(1, 1, &[0; 4], 3)
            .expect("invariant: 1x1 RGBA8")
            .with_light(Some(light));
        let centre = |graph: &Graph| {
            let shot = snapshot::plan_with(
                graph,
                Some(&SceneId::new("main")),
                None,
                &mut |id| {
                    Some(Look::frame(
                        if id.as_str() == "cam" {
                            white.clone()
                        } else {
                            lamp.clone()
                        },
                        None,
                    ))
                },
                &FilterAssets::default(),
            )
            .shot;
            let _gpu = crate::tests::one_gpu_test_at_a_time();
            let mut compositor = gpu()?;
            let mut cache = ResourceCache::new();
            let mut chains = Chains::default();
            Some(
                draw_points(
                    &mut compositor,
                    (&mut cache, &mut chains),
                    (&shot, &LiveParams::default()),
                    &[(32, 18)],
                )[0],
            )
        };
        let Some(unlit) = centre(&graph) else {
            return;
        };
        assert!(
            unlit[..3].iter().all(|&c| c > 245),
            "unlit, it is white: {unlit:?}"
        );
        apply(&mut graph, vec![lit(true)]);
        let red = centre(&graph).expect("an adapter");
        assert!(
            red[0] > 60 && red[1] < 20 && red[2] < 20,
            "the red light shades it: {red:?}"
        );
        apply(&mut graph, vec![hide("logo")]);
        let dark = centre(&graph).expect("an adapter");
        assert!(
            dark[..3].iter().all(|&c| c > 245),
            "with no light it draws unlit: {dark:?}"
        );
    }

    /// ADR-0151 decision 4, end to end: a card lying flat as the floor, and a red card
    /// standing on it, through the framing camera. The floor under the red card shows it
    /// reflected: the mirrored pass leaves out the floor itself, which would otherwise
    /// hide everything above it from the mirrored camera.
    #[test]
    fn a_floor_card_reflects_what_stands_on_it() {
        let canvas = jstream_protocol::Canvas {
            width: CANVAS.0,
            height: CANVAS.1,
        };
        let mut graph = placed(&[
            ("cam", (32.0, 30.0), (400.0, 400.0), 0.0),
            ("logo", (32.0, 27.0), (24.0, 6.0), 0.0),
        ]);
        let mut floor = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 30.0).expect("finite"),
            jstream_protocol::Size::try_new(400.0, 400.0).expect("positive"),
        )
        .expect("valid");
        floor.fit_mode = jstream_protocol::FitMode::Stretch;
        floor.tilt_deg = jstream_protocol::Finite32::new(90.0).expect("finite");
        let mirror = |strength: f32| Edit::FilterAdd {
            owner: FilterOwner::Scene {
                scene_id: SceneId::new("main"),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new(format!("mirror{strength}")),
            kind: "floor_reflection".into(),
            settings: [
                ("floor_y_px", serde_json::json!(30.0)),
                ("strength", serde_json::json!(strength)),
            ]
            .into_iter()
            .collect(),
            enabled: Some(true),
            index: None,
        };
        apply(
            &mut graph,
            vec![
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("cam"),
                    geometry: floor,
                },
                Edit::SceneSetCamera {
                    scene_id: SceneId::new("main"),
                    camera: Some(jstream_protocol::SceneCamera::framing(canvas)),
                },
            ],
        );
        let grey =
            Frame::new(16, 9, &[64, 64, 64, 255].repeat(16 * 9), 2).expect("invariant: 16x9 RGBA8");
        let red =
            Frame::new(16, 9, &[255, 0, 0, 255].repeat(16 * 9), 3).expect("invariant: 16x9 RGBA8");
        let under = |graph: &Graph| {
            let shot = snapshot::plan_with(
                graph,
                Some(&SceneId::new("main")),
                None,
                &mut |id| {
                    Some(Look::frame(
                        if id.as_str() == "cam" {
                            grey.clone()
                        } else {
                            red.clone()
                        },
                        None,
                    ))
                },
                &FilterAssets::default(),
            )
            .shot;
            let _gpu = crate::tests::one_gpu_test_at_a_time();
            let mut compositor = gpu()?;
            let mut cache = ResourceCache::new();
            let mut chains = Chains::default();
            Some(
                draw_points(
                    &mut compositor,
                    (&mut cache, &mut chains),
                    (&shot, &LiveParams::default()),
                    &[(32, 33)],
                )[0],
            )
        };
        let Some(bare) = under(&graph) else {
            return;
        };
        assert!(
            bare[0].abs_diff(bare[1]) < 8,
            "the floor alone is grey under the card: {bare:?}"
        );
        apply(&mut graph, vec![mirror(1.0)]);
        let reflected = under(&graph).expect("an adapter");
        assert!(
            reflected[0] > reflected[1] + 40,
            "the red card is reflected in the floor under it: {reflected:?}"
        );
    }

    /// ADR-0170 decision 4, end to end: a mirror-smooth metal block, a model lit by its
    /// scene, its top a floor under a red card standing on it, through the framing
    /// camera. With the
    /// scene's reflections on, the floor under the card shows it, which only the screen
    /// shows: no probe sees it. With the switch off, or the scene's reflections off, the
    /// floor reflects its environment only.
    #[test]
    fn a_metal_floor_reflects_a_card_on_screen() {
        use jstream_sources::SourceKind as _;
        let canvas = jstream_protocol::Canvas {
            width: CANVAS.0,
            height: CANVAS.1,
        };
        let mut graph = placed(&[
            ("cam", (32.0, 50.0), (40.0, 40.0), 0.0),
            ("logo", (32.0, 25.0), (24.0, 6.0), 0.0),
        ]);
        // The card stands on the block's top, a little nearer than its middle, so it
        // is drawn over the block.
        let mut card = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 25.0).expect("finite"),
            jstream_protocol::Size::try_new(24.0, 6.0).expect("positive"),
        )
        .expect("valid");
        card.fit_mode = jstream_protocol::FitMode::Stretch;
        card.depth_px = jstream_protocol::Finite32::new(5.0).expect("finite");
        let reflective = |on: bool| Edit::SceneSetOptions {
            scene_id: SceneId::new("main"),
            options: jstream_protocol::SceneOptions {
                reflections: on,
                ..jstream_protocol::SceneOptions::default()
            },
        };
        apply(
            &mut graph,
            vec![
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("logo"),
                    geometry: card,
                },
                Edit::SceneSetCamera {
                    scene_id: SceneId::new("main"),
                    camera: Some(jstream_protocol::SceneCamera::framing(canvas)),
                },
                reflective(true),
            ],
        );
        let kind = jstream_sources::ModelKind::new();
        let mut settings: jstream_protocol::Settings = kind
            .descriptor()
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect();
        settings.insert(String::from("shape"), serde_json::json!("cube"));
        settings.insert(String::from("lighting"), serde_json::json!("scene"));
        settings.insert(String::from("color"), serde_json::json!("#ffffff"));
        settings.insert(String::from("metallic"), serde_json::json!(1.0));
        settings.insert(String::from("roughness"), serde_json::json!(0.0));
        let model = kind
            .create(&settings, &jstream_sources::Context::default())
            .expect("a model");
        let stand_in = Frame::new(16, 9, &[0; 16 * 9 * 4], 2)
            .expect("invariant: 16x9 RGBA8")
            .with_model(model.model());
        let red =
            Frame::new(16, 9, &[255, 0, 0, 255].repeat(16 * 9), 3).expect("invariant: 16x9 RGBA8");
        let under_at = |graph: &Graph, features: jstream_protocol::RenderFeatures, percent| {
            let assets = FilterAssets::default().with_features(features);
            let shot = snapshot::plan_with(
                graph,
                Some(&SceneId::new("main")),
                None,
                &mut |id| {
                    Some(Look::frame(
                        if id.as_str() == "cam" {
                            stand_in.clone()
                        } else {
                            red.clone()
                        },
                        None,
                    ))
                },
                &assets,
            )
            .shot;
            let _gpu = crate::tests::one_gpu_test_at_a_time();
            let mut compositor = gpu()?;
            let mut cache = ResourceCache::new();
            let mut chains = Chains {
                model_percent: crate::resolution::Percent::of(percent),
                ..Chains::default()
            };
            let point = draw_points(
                &mut compositor,
                (&mut cache, &mut chains),
                (&shot, &LiveParams::default()),
                &[(32, 35)],
            )[0];
            // One model, drawn once through the scene's camera: its layer, view depth
            // and surface come from that one draw, whatever reads them.
            assert_eq!(
                chains.scene_model_draws.get(),
                1,
                "the model is drawn once at {percent}%"
            );
            Some((point, !chains.surfaced.is_empty()))
        };
        let under = |graph: &Graph, features| under_at(graph, features, 100);
        let all = jstream_protocol::RenderFeatures::ALL;
        let Some((reflected, surfaced)) = under(&graph, all) else {
            return;
        };
        assert!(
            surfaced,
            "the floor's surface is drawn with its layer, not again"
        );
        let off = jstream_protocol::RenderFeatures {
            screen_space_reflections: false,
            ..all
        };
        let (plain, surfaced) = under(&graph, off).expect("an adapter");
        assert!(!surfaced, "no surface without the pass");
        assert!(
            u16::from(reflected[0]) > u16::from(plain[0]) + 40
                && u16::from(reflected[0]) > u16::from(reflected[1]) + 40,
            "the red card is reflected in the floor under it: {reflected:?} (without: {plain:?})"
        );
        // The floor drawn with half the pixels each way (ADR-0197): its surface is still
        // drawn with its layer, spread over the scene's buffer, and reflects the card.
        let (half, surfaced) = under_at(&graph, all, 50).expect("an adapter");
        assert!(surfaced, "the smaller surface is merged, not drawn again");
        assert!(
            u16::from(half[0]) > u16::from(plain[0]) + 40
                && u16::from(half[0]) > u16::from(half[1]) + 40,
            "the red card is reflected in the floor drawn smaller: {half:?} (without: {plain:?})"
        );
        apply(&mut graph, vec![reflective(false)]);
        let (unreflective, _) = under(&graph, all).expect("an adapter");
        assert_eq!(unreflective, plain, "a scene without reflections");
    }

    /// ADR-0151 decision 6: a scene whose chain holds volumetric fog hands it the shadow
    /// map of a spot light that casts shadows, drawn by the scene's pass when no model
    /// receives it; a light that casts none hands it no map.
    #[test]
    fn volumetric_fog_gets_the_shadow_maps_of_its_scene() {
        let canvas = jstream_protocol::Canvas {
            width: CANVAS.0,
            height: CANVAS.1,
        };
        let mut graph = placed(&[("cam", (32.0, 18.0), (64.0, 36.0), 0.0)]);
        let mut lamp = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(8.0, 8.0).expect("positive"),
        )
        .expect("valid");
        lamp.depth_px = jstream_protocol::Finite32::new(20.0).expect("finite");
        let fog = Edit::FilterAdd {
            owner: FilterOwner::Scene {
                scene_id: SceneId::new("main"),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new("fog"),
            kind: "volumetric_fog".into(),
            settings: [("density", serde_json::json!(1.0))].into_iter().collect(),
            enabled: Some(true),
            index: None,
        };
        apply(
            &mut graph,
            vec![
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("logo"),
                    geometry: lamp,
                },
                fog,
                Edit::SceneSetCamera {
                    scene_id: SceneId::new("main"),
                    camera: Some(jstream_protocol::SceneCamera::framing(canvas)),
                },
            ],
        );
        let light = |shadows| jstream_sources::ShownLight {
            kind: jstream_sources::LightType::Spot,
            color: [1.0; 3],
            intensity: 4.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows,
            shadow_softness: 0.3,
            environment: None,
        };
        let black =
            Frame::new(16, 9, &[0, 0, 0, 255].repeat(16 * 9), 2).expect("invariant: 16x9 RGBA8");
        let maps = |shadows: bool| {
            let lamp = Frame::new(1, 1, &[0; 4], 3)
                .expect("invariant: 1x1 RGBA8")
                .with_light(Some(light(shadows)));
            let shot = snapshot::plan_with(
                &graph,
                Some(&SceneId::new("main")),
                None,
                &mut |id| {
                    Some(Look::frame(
                        if id.as_str() == "cam" {
                            black.clone()
                        } else {
                            lamp.clone()
                        },
                        None,
                    ))
                },
                &FilterAssets::default(),
            )
            .shot;
            let _gpu = crate::tests::one_gpu_test_at_a_time();
            let mut compositor = gpu()?;
            let mut cache = ResourceCache::new();
            let mut chains = Chains::default();
            draw_points(
                &mut compositor,
                (&mut cache, &mut chains),
                (&shot, &LiveParams::default()),
                &[(32, 18)],
            );
            let depth = &chains.depths.get(&SceneId::new("main"))?.1;
            Some(
                depth
                    .shadows
                    .iter()
                    .map(|map| (map.light, map.image.width()))
                    .collect::<Vec<_>>(),
            )
        };
        let Some(cast) = maps(true) else {
            return;
        };
        assert_eq!(
            cast,
            [(0, jstream_render::SHADOW_MAP_PX)],
            "the lamp's map reaches the fog"
        );
        assert_eq!(maps(false), Some(Vec::new()), "no shadows, no map");
    }

    /// A 3D scene for shadows: `cam`, a wall behind the canvas plane that a model source
    /// shows; `logo`, a lamp in front of it turned 30 degrees to one side; and `card`, a
    /// small card on the canvas plane between them, moved `nudge` px across.
    fn shadow_wall(nudge: f32) -> Graph {
        let mut graph = placed(&[("cam", (32.0, 18.0), (96.0, 72.0), 0.0)]);
        let main = SceneId::new("main");
        let geometry = |(x, y): (f32, f32), size: f32, depth: f32, turn: f32| {
            let mut g = jstream_protocol::ItemGeometry::try_new(
                jstream_protocol::Vec2::new(x, y).expect("finite"),
                jstream_protocol::Size::try_new(size, size).expect("positive"),
            )
            .expect("valid");
            g.depth_px = jstream_protocol::Finite32::new(depth).expect("finite");
            g.turn_deg = jstream_protocol::Finite32::new(turn).expect("finite");
            g
        };
        let mut wall = geometry((32.0, 18.0), 96.0, -30.0, 0.0);
        wall.fit_mode = jstream_protocol::FitMode::Stretch;
        apply(
            &mut graph,
            vec![
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("cam"),
                    geometry: wall,
                },
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("logo"),
                    geometry: geometry((32.0, 18.0), 4.0, 0.0, 30.0),
                },
                Edit::SourceCreate {
                    source_id: SourceId::new("card"),
                    name: "card".into(),
                    kind: SourceKind::Builtin {
                        kind: "display".into(),
                    },
                    settings: Default::default(),
                    duplicated_from: None,
                },
                Edit::ItemAdd {
                    scene_id: main.clone(),
                    item_id: ItemId::new("card"),
                    source_id: SourceId::new("card"),
                    transform: None,
                    geometry: Some(geometry((32.0 + nudge, 18.0), 12.0, 0.0, 0.0)),
                    opacity: None,
                    blend: None,
                },
                Edit::SceneSetCamera {
                    scene_id: main,
                    camera: Some(jstream_protocol::SceneCamera::framing(
                        jstream_protocol::Canvas {
                            width: CANVAS.0,
                            height: CANVAS.1,
                        },
                    )),
                },
            ],
        );
        graph
    }

    /// The directional light [`shadow_wall`]'s lamp shows, casting shadows or not.
    fn shadow_wall_light(shadows: bool) -> jstream_sources::ShownLight {
        jstream_sources::ShownLight {
            kind: jstream_sources::LightType::Directional,
            color: [1.0; 3],
            intensity: 2.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows,
            shadow_softness: 0.0,
            environment: None,
        }
    }

    /// The model [`shadow_wall`]'s wall shows: a white plane lit by its scene, which
    /// `receives` shadows or doesn't.
    fn shadow_wall_model(receives: bool) -> Option<jstream_sources::ShownModel> {
        use jstream_sources::SourceKind as _;
        let kind = jstream_sources::ModelKind::new();
        let mut settings: jstream_protocol::Settings = kind
            .descriptor()
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect();
        settings.insert(String::from("shape"), serde_json::json!("plane"));
        settings.insert(String::from("lighting"), serde_json::json!("scene"));
        settings.insert(String::from("color"), serde_json::json!("#ffffff"));
        settings.insert(String::from("roughness"), serde_json::json!(1.0));
        settings.insert(
            String::from("receives_shadows"),
            serde_json::json!(receives),
        );
        kind.create(&settings, &jstream_sources::Context::default())
            .expect("a model")
            .model()
    }

    /// `graph`, a [`shadow_wall`], planned: its wall showing `model`, its lamp `light`,
    /// and its card the frame `card`.
    fn shadow_wall_shot(
        graph: &Graph,
        model: Option<jstream_sources::ShownModel>,
        light: jstream_sources::ShownLight,
        card: &Frame,
    ) -> Shot {
        let stand_in = Frame::new(16, 9, &[0; 16 * 9 * 4], 2)
            .expect("invariant: 16x9 RGBA8")
            .with_model(model);
        let lamp = Frame::new(1, 1, &[0; 4], 3)
            .expect("invariant: 1x1 RGBA8")
            .with_light(Some(light));
        snapshot::plan_with(
            graph,
            Some(&SceneId::new("main")),
            None,
            &mut |id| {
                Some(Look::frame(
                    match id.as_str() {
                        "cam" => stand_in.clone(),
                        "logo" => lamp.clone(),
                        _ => card.clone(),
                    },
                    None,
                ))
            },
            &FilterAssets::default(),
        )
        .shot
    }

    /// ADR-0151 decision 3: a card between a directional light that casts shadows and a
    /// `scene`-lit wall shadows the wall, through the shadow map the first model that
    /// receives them draws; without shadows, or for a wall that receives none, the wall
    /// is lit there.
    #[test]
    fn a_card_shadows_a_scene_lit_wall() {
        let mut graph = shadow_wall(0.0);
        // The card's shadow falls 17 px to one side of it on the wall, which is behind
        // the canvas plane, so the camera sees it nearer the centre: about 10 px out
        // from the card's edge.
        let sides = [(22, 18), (42, 18)];
        let shot = |graph: &Graph, light: jstream_sources::ShownLight, receives: bool| {
            let card = Frame::new(1, 1, &[255; 4], 4).expect("invariant: 1x1 RGBA8");
            let shot = shadow_wall_shot(graph, shadow_wall_model(receives), light, &card);
            let _gpu = crate::tests::one_gpu_test_at_a_time();
            let mut compositor = gpu()?;
            let mut cache = ResourceCache::new();
            let mut chains = Chains::default();
            Some(draw_points(
                &mut compositor,
                (&mut cache, &mut chains),
                (&shot, &LiveParams::default()),
                &sides,
            ))
        };
        let light = shadow_wall_light;
        let luma = |pixel: [u8; 4]| pixel[..3].iter().map(|&c| u32::from(c)).sum::<u32>();
        let Some(lit) = shot(&graph, light(false), true) else {
            return;
        };
        let shadowed = shot(&graph, light(true), true).expect("an adapter");
        let unreceived = shot(&graph, light(true), false).expect("an adapter");
        assert!(
            lit.iter().all(|&p| luma(p) > 150),
            "the wall is lit: {lit:?}"
        );
        assert_eq!(unreceived, lit, "a wall that receives none is lit");
        // The card's shadow falls to one side of it.
        let darker = (0..2)
            .filter(|&side| luma(shadowed[side]) + 100 < luma(lit[side]))
            .count();
        let same = (0..2)
            .filter(|&side| luma(shadowed[side]).abs_diff(luma(lit[side])) <= 6)
            .count();
        assert_eq!(
            (darker, same),
            (1, 1),
            "one side is shadowed: {shadowed:?} against {lit:?}"
        );
        // ADR-0179: a card that only adds light casts no shadow.
        for blend in [
            jstream_protocol::BlendMode::Additive,
            jstream_protocol::BlendMode::Screen,
        ] {
            apply(
                &mut graph,
                vec![Edit::ItemSetBlend {
                    item_id: ItemId::new("card"),
                    blend,
                }],
            );
            let light_only = shot(&graph, light(true), true).expect("an adapter");
            assert!(
                (0..2).all(|side| luma(light_only[side]).abs_diff(luma(lit[side])) <= 6),
                "a {blend:?} card casts no shadow: {light_only:?} against {lit:?}"
            );
        }
    }

    /// `profile.get`'s GPU parts: a model that receives shadows, under an effect, comes
    /// back with its shadow maps, its draw and its effect timed apart, each inside its
    /// item's GPU time, on a device that times the GPU. Its view depth is read from its
    /// draw's tested depth, so it is in the draw's part, and no part of its own.
    #[test]
    fn a_models_gpu_time_comes_back_in_parts() {
        use crate::profile::{InputTotals, Profiler, SourceMeters};

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        if !compositor.times_gpu() {
            return;
        }
        let mut graph = shadow_wall(0.0);
        apply(&mut graph, vec![item_tint("cam", "#ff0000")]);
        let card = Frame::new(1, 1, &[255; 4], 4).expect("invariant: 1x1 RGBA8");
        let shot = shadow_wall_shot(
            &graph,
            shadow_wall_model(true),
            shadow_wall_light(true),
            &card,
        );
        let profiler = Profiler::new(
            std::time::Duration::from_millis(33),
            std::sync::Arc::new(SourceMeters::default()),
        );
        let mut meter = profiler.program_meter();
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let canvas = shot.canvas().expect("canvas");
        let started = std::time::Instant::now();
        // The first frame is timed; the ones after collect its times, each read back,
        // so the GPU is done with the one before.
        for frame in 0..4u64 {
            cache.begin_frame();
            meter.begin_gpu(&mut compositor);
            snapshot::compose_metered(
                &mut compositor,
                (&mut cache, &mut chains, &mut meter),
                (&shot, MediaTime::zero(Timebase::MILLIS)),
                snapshot::View::default(),
                &LiveParams::default(),
                &NoCaptures,
                snapshot::DrawAt::read(canvas),
            )
            .expect("compose");
            meter.end_gpu(&mut compositor);
            drop(cache.end_frame(&compositor));
            meter.end_frame(
                std::time::Duration::from_millis(1),
                started + std::time::Duration::from_secs(frame),
                drop,
            );
        }
        let info = profiler.profile(Some(60), None, None, InputTotals::default());
        let unit = info
            .units
            .iter()
            .find(|unit| {
                matches!(&unit.unit, jstream_protocol::ProfileUnit::ItemEffects { item_id, .. }
                    if item_id.as_str() == "cam")
            })
            .expect("the wall's effects");
        let parts: Vec<_> = unit.gpu_parts.iter().map(|part| &part.part).collect();
        use jstream_protocol::GpuPart;
        for part in [
            GpuPart::ShadowMaps,
            GpuPart::Model,
            GpuPart::Filter { index: 0 },
        ] {
            assert!(parts.contains(&&part), "{part:?} is timed: {unit:?}");
        }
        assert!(
            !parts.contains(&&GpuPart::ViewDepth),
            "the view depth is the draw's: {unit:?}"
        );
        let whole = unit.gpu_ms.expect("timed").get();
        let sum: f32 = unit.gpu_parts.iter().map(|part| part.gpu_ms.get()).sum();
        assert!(
            sum <= whole + 0.001,
            "the parts are inside the whole: {unit:?}"
        );
    }

    /// A shadow map is drawn once while nothing that casts into it changes: frames of
    /// the same scene leave it as it is and show the same shadow; a card that shows a
    /// new picture, or moves, draws it again.
    #[test]
    fn a_shadow_map_is_drawn_again_only_when_what_casts_changes() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let sides = [(22, 18), (42, 18)];
        let model = shadow_wall_model(true);
        let card = |generation| Frame::new(1, 1, &[255; 4], generation).expect("1x1 RGBA8");
        let mut frame = |graph: &Graph, card: &Frame, chains: &mut Chains| {
            let shot = shadow_wall_shot(graph, model.clone(), shadow_wall_light(true), card);
            let pixels = draw_points(
                &mut compositor,
                (&mut cache, &mut *chains),
                (&shot, &LiveParams::default()),
                &sides,
            );
            (pixels, chains.shadow_draws.get())
        };
        let still = shadow_wall(0.0);
        let (first, draws) = frame(&still, &card(4), &mut chains);
        assert_eq!(draws, 1, "the map is drawn");
        let (again, draws) = frame(&still, &card(4), &mut chains);
        assert_eq!(draws, 1, "nothing changed: the map is kept");
        assert_eq!(again, first, "and shows the same shadow");
        let (_, draws) = frame(&still, &card(5), &mut chains);
        assert_eq!(draws, 2, "the card shows a new picture");
        let (_, draws) = frame(&still, &card(5), &mut chains);
        assert_eq!(draws, 2, "which is then kept");
        let (moved, draws) = frame(&shadow_wall(20.0), &card(5), &mut chains);
        assert_eq!(draws, 3, "the card moved");
        assert_ne!(moved, first, "and its shadow with it");
    }

    /// ADR-0151 decision 3: the shadow maps a frame draws hold every model that casts,
    /// whichever model is drawn first. Here the card is a model too, a cube that casts
    /// onto the wall, in one stacking order and then the other: the first frame a
    /// compositor draws shows the cube's shadow on the wall, as every frame after it
    /// does. Before the models the scene shows were made ready for the maps, the wall
    /// drawn first drew them without the cube, which had no upload or layer yet, so the
    /// first frame showed the wall unshadowed.
    #[test]
    fn the_first_frame_shadows_with_every_model() {
        first_frame_shadows(false);
    }

    /// As [`the_first_frame_shadows_with_every_model`], with effects on the cube's item:
    /// its layer is then its effects' output, which isn't made until its chain runs,
    /// after the wall drawn first drew the maps. The maps take the cube's placement
    /// without that layer, so its first frame shadows the wall too (#1674).
    #[test]
    fn the_first_frame_shadows_with_a_model_with_effects() {
        first_frame_shadows(true);
    }

    /// The body of the two tests above: with `effects`, the cube's item has an effect
    /// that leaves its picture as it is.
    fn first_frame_shadows(effects: bool) {
        use jstream_sources::SourceKind as _;
        let sides = [(22, 18), (42, 18)];
        let kind = jstream_sources::ModelKind::new();
        let mut settings: jstream_protocol::Settings = kind
            .descriptor()
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect();
        settings.insert(String::from("shape"), serde_json::json!("cube"));
        settings.insert(String::from("lighting"), serde_json::json!("scene"));
        let cube = kind
            .create(&settings, &jstream_sources::Context::default())
            .expect("a model")
            .model();
        let block = Frame::new(1, 1, &[255; 4], 4)
            .expect("invariant: 1x1 RGBA8")
            .with_model(cube);
        let luma = |pixel: [u8; 4]| pixel[..3].iter().map(|&c| u32::from(c)).sum::<u32>();
        for on_top in [true, false] {
            let mut graph = shadow_wall(0.0);
            if effects {
                apply(&mut graph, vec![item_tint("card", "#ffffff")]);
            }
            if !on_top {
                apply(
                    &mut graph,
                    vec![Edit::ItemReorder {
                        item_id: ItemId::new("card"),
                        index: 0,
                    }],
                );
            }
            let shot = shadow_wall_shot(
                &graph,
                shadow_wall_model(true),
                shadow_wall_light(true),
                &block,
            );
            let _gpu = crate::tests::one_gpu_test_at_a_time();
            let Some(mut compositor) = gpu() else { return };
            let mut cache = ResourceCache::new();
            let mut chains = Chains::default();
            let mut frame = || {
                draw_points(
                    &mut compositor,
                    (&mut cache, &mut chains),
                    (&shot, &LiveParams::default()),
                    &sides,
                )
            };
            let first = frame();
            let second = frame();
            assert_eq!(
                first, second,
                "the first frame shows what the next does (cube on top: {on_top}, effects: {effects})"
            );
            assert!(
                luma(first[0]).abs_diff(luma(first[1])) > 100,
                "the cube shadows one side of the wall (cube on top: {on_top}, effects: {effects}): {first:?}"
            );
        }
    }

    /// A scene primed on the snapshot thread leaves the program stage nothing to compile
    /// (`SnapshotWorker::prime`): a compositor opened on the same context afterwards, as
    /// an output start opens one, draws the scene, its model, shadow map and filter,
    /// and makes no pipeline. What a 3D scene alone draws with is made too, whether or
    /// not the scene shows it yet.
    #[test]
    fn a_primed_scene_leaves_the_stage_no_pipeline_to_make() {
        use crate::runtime::SharedGpu;
        use jstream_platform::{AdapterId, GpuContext, GpuFrame, GpuInterop, PlatformError};

        /// Lends the context it was made with.
        struct Lent(GpuContext);
        impl GpuInterop for Lent {
            fn open_device(&self) -> Result<GpuContext, PlatformError> {
                Ok(self.0.clone())
            }
            fn encode_target(
                &self,
                _: &GpuContext,
                _: FrameSize,
            ) -> Result<GpuFrame, PlatformError> {
                Err(PlatformError::Unsupported {
                    what: "encode targets".into(),
                })
            }
        }

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(opened) = gpu() else { return };
        let context = GpuContext::new(
            opened.adapter().clone(),
            opened.device().clone(),
            opened.queue().clone(),
            AdapterId {
                luid: None,
                vendor: 0,
            },
            None,
        );
        drop(opened);
        let mut graph = shadow_wall(0.0);
        apply(
            &mut graph,
            vec![Edit::FilterAdd {
                owner: FilterOwner::Source {
                    source_id: SourceId::new("card"),
                },
                chain: FilterChain::Video,
                filter_id: FilterId::new("grade"),
                kind: "color_correction".into(),
                settings: [("multiply".to_owned(), serde_json::json!("#ffffff"))]
                    .into_iter()
                    .collect(),
                enabled: None,
                index: None,
            }],
        );
        let card = Frame::new(1, 1, &[255; 4], 4).expect("1x1 RGBA8");
        let shot = || {
            shadow_wall_shot(
                &graph,
                shadow_wall_model(true),
                shadow_wall_light(true),
                &card,
            )
        };
        let shared = Arc::new(SharedGpu::default());
        shared.set_interop(Arc::new(Lent(context.clone())));
        let worker = snapshot::SnapshotWorker::new(Arc::clone(&shared));
        worker.prime(shot(), MediaTime::zero(Timebase::MILLIS));
        // Dropping the worker joins its thread once the scene is drawn.
        drop(worker);

        let mut stage = Compositor::on_context(&context);
        let made = stage.pipelines_made();
        assert!(made > 0, "priming made the scene's pipelines");
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        for _ in 0..2 {
            draw_points(
                &mut stage,
                (&mut cache, &mut chains),
                (&shot(), &LiveParams::default()),
                &[(22, 18)],
            );
        }
        stage.warm_3d();
        assert_eq!(stage.pipelines_made(), made, "the stage made none");
    }

    /// A card whose picture comes out of a filter chain casts the shadow of what the
    /// filter drew this frame: the chain's output layer is one image for as long as it
    /// lives, and each run of the filter counts as a change of it, so the kept map is
    /// drawn again. Here the card's shadow comes once the filter has drawn it, and goes
    /// once its picture has turned transparent.
    #[test]
    fn a_filtered_cards_shadow_follows_its_picture() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let sides = [(22, 18), (42, 18)];
        let mut graph = shadow_wall(0.0);
        apply(
            &mut graph,
            vec![Edit::FilterAdd {
                owner: FilterOwner::Source {
                    source_id: SourceId::new("card"),
                },
                chain: FilterChain::Video,
                filter_id: FilterId::new("grade"),
                kind: "color_correction".into(),
                settings: [("multiply".to_owned(), serde_json::json!("#ffffff"))]
                    .into_iter()
                    .collect(),
                enabled: None,
                index: None,
            }],
        );
        let model = shadow_wall_model(true);
        let luma = |pixel: [u8; 4]| pixel[..3].iter().map(|&c| u32::from(c)).sum::<u32>();
        let mut frame = |card: &Frame, chains: &mut Chains| {
            let shot = shadow_wall_shot(&graph, model.clone(), shadow_wall_light(true), card);
            let pixels = draw_points(
                &mut compositor,
                (&mut cache, &mut *chains),
                (&shot, &LiveParams::default()),
                &sides,
            );
            (pixels, chains.shadow_draws.get())
        };
        let solid = Frame::new(1, 1, &[255; 4], 4).expect("1x1 RGBA8");
        let clear = Frame::new(1, 1, &[0; 4], 5).expect("1x1 RGBA8");
        let darkest = |pixels: &[[u8; 4]]| pixels.iter().map(|&p| luma(p)).min().unwrap_or(0);
        // The maps are drawn before the card's chain runs, so they show the picture
        // the filter drew the frame before: nothing yet in the first frame.
        let (unshadowed, draws) = frame(&solid, &mut chains);
        assert_eq!(draws, 1);
        let (shadowed, draws) = frame(&solid, &mut chains);
        assert_eq!(draws, 2, "the filter has drawn the card since");
        assert!(
            darkest(&shadowed) + 100 < darkest(&unshadowed),
            "the card casts a shadow: {shadowed:?} after {unshadowed:?}"
        );
        frame(&clear, &mut chains);
        let (lit, draws) = frame(&clear, &mut chains);
        assert_eq!(draws, 4, "the filter drew a new picture");
        assert_eq!(lit, unshadowed, "a card that shows nothing casts none");
    }

    /// ADR-0143: a scene-lit model is lit by its scene's lights while a transition
    /// switches to that scene too, not only once it has landed.
    #[test]
    fn a_scene_lit_model_stays_lit_through_a_transition() {
        let mut graph = placed(&[("cam", (32.0, 18.0), (32.0, 36.0), 0.0)]);
        let main = SceneId::new("main");
        let mut geometry = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(8.0, 8.0).expect("positive"),
        )
        .expect("valid");
        geometry.depth_px = jstream_protocol::Finite32::new(200.0).expect("finite");
        apply(
            &mut graph,
            vec![
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("logo"),
                    geometry,
                },
                Edit::SceneSetCamera {
                    scene_id: main.clone(),
                    camera: Some(jstream_protocol::SceneCamera::framing(
                        jstream_protocol::Canvas {
                            width: CANVAS.0,
                            height: CANVAS.1,
                        },
                    )),
                },
            ],
        );
        let light = jstream_sources::ShownLight {
            kind: jstream_sources::LightType::Point,
            color: [1.0; 3],
            intensity: 2.0,
            range_px: 0.0,
            cone_deg: 45.0,
            softness: 0.2,
            shadows: false,
            shadow_softness: 0.3,
            environment: None,
        };
        let running = crate::transition::Running {
            from: crate::transition::From::Scene(main.clone()),
            to: main.clone(),
            start: MediaTime::zero(Timebase::MILLIS),
            run: crate::transition::Run {
                preset_id: jstream_protocol::TransitionPresetId::new("fade"),
                pairs: true,
                duration_ms: 500,
                easing: jstream_protocol::Easing::Linear,
                unmatched: jstream_protocol::TransitionEffect::None,
                stagger_ms: 0,
                stinger: None,
                orbit: false,
            },
            by_item: true,
            from_camera: crate::transition::camera_of(&graph, &main),
            via: Vec::new(),
            in_place: false,
            carried: Default::default(),
        };
        let luma = |pixel: [u8; 4]| pixel[..3].iter().map(|&c| u32::from(c)).sum::<u32>();
        let Some(landed) = lit_centre(&graph, light.clone()) else {
            return;
        };
        let switching = lit_centre_in(&graph, light, Some(&running)).expect("an adapter");
        assert!(luma(landed) > 150, "lit once landed: {landed:?}");
        assert!(
            luma(switching) > 150,
            "lit while switching: {switching:?} (landed {landed:?})"
        );
    }

    /// While the GPU is short of time (`Chains::economy`) a frame's models are drawn with
    /// one sample a pixel, whatever the rendering switches ask, and with the samples
    /// they ask again after it.
    #[test]
    fn in_economy_models_are_drawn_with_one_sample() {
        let mut graph = placed(&[("cam", (32.0, 18.0), (32.0, 36.0), 0.0)]);
        apply(
            &mut graph,
            vec![
                hide("logo"),
                Edit::SceneSetCamera {
                    scene_id: SceneId::new("main"),
                    camera: Some(jstream_protocol::SceneCamera::framing(
                        jstream_protocol::Canvas {
                            width: CANVAS.0,
                            height: CANVAS.1,
                        },
                    )),
                },
            ],
        );
        let shot = model_shot(&graph, "plane");
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains {
            economy: true,
            ..Chains::default()
        };
        let mut samples = |chains: &mut Chains| {
            draw_points(
                &mut compositor,
                (&mut cache, chains),
                (&shot, &LiveParams::default()),
                &[(32, 18)],
            );
            compositor.model_antialiasing()
        };
        assert_eq!(samples(&mut chains), 1);
        chains.economy = false;
        // The default (ADR-0242).
        assert_eq!(samples(&mut chains), 4);
    }

    /// In a 3D scene a model is seen through the scene's camera (ADR-0107 decision 6): a
    /// plane turned 60° about its vertical axis is half as wide, about its own centre.
    #[test]
    fn golden_a_model_is_seen_through_the_scene_camera() {
        let mut graph = placed(&[("cam", (32.0, 18.0), (32.0, 36.0), 0.0)]);
        let mut geometry = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(32.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(32.0, 36.0).expect("positive"),
        )
        .expect("valid");
        geometry.fit_mode = jstream_protocol::FitMode::Stretch;
        geometry.turn_deg = jstream_protocol::Finite32::new(60.0).expect("finite");
        apply(
            &mut graph,
            vec![
                hide("logo"),
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("cam"),
                    geometry,
                },
                Edit::SceneSetCamera {
                    scene_id: SceneId::new("main"),
                    camera: Some(jstream_protocol::SceneCamera::framing(
                        jstream_protocol::Canvas {
                            width: CANVAS.0,
                            height: CANVAS.1,
                        },
                    )),
                },
            ],
        );
        let expected = "
            .....WWWW.......
            .....WWWW.......
            .....WWWW.......
            .....WWWW.......
            .....WWWW.......
            .....WWWW.......
            .....WWWW.......
            .....WWWW.......
            .....WWW?.......
            ";
        // The last row's `?` is the plane's slanted near corner, which the model's
        // 4x multisampling (ADR-0242, the default) blends with the black around it.
        assert_golden(
            &model_shot(&graph, "plane"),
            expected,
            "turned 60°, the plane is about half as wide, its near edge the longer",
        );
        // Drawn with half the pixels each way (ADR-0197), its layer is half the canvas's
        // size, stands for the whole canvas, and shows the plane in the same place: only
        // the cells its edge crosses (`?`) differ, softened by the enlarging.
        if let Some((half, layer)) = golden_at(&model_shot(&graph, "plane"), 50) {
            assert_eq!(
                layer,
                Some([(CANVAS.0 / 2, CANVAS.1 / 2), CANVAS]),
                "the layer's pixels and the canvas it is placed over"
            );
            let full: String = expected
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .flat_map(|line| line.chars().chain(['\n']))
                .collect();
            assert!(
                half.len() == full.len()
                    && half
                        .chars()
                        .zip(full.chars())
                        .all(|(h, f)| h == f || h == '?'),
                "at half the pixels the plane covers the same cells\nhalf:\n{half}"
            );
            assert!(
                half.chars().filter(|c| *c == 'W').count() >= 24,
                "and its inside is as white\nhalf:\n{half}"
            );
        }
        // A flat item in the same pose covers the same pixels.
        apply(
            &mut graph,
            vec![
                hide("cam"),
                Edit::ItemSetVisible {
                    scene_id: None,
                    item_id: ItemId::new("logo"),
                    visible: true,
                },
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("logo"),
                    geometry,
                },
            ],
        );
        // The flat item's edge is antialiased where the model's isn't: a cell between
        // the two (`?`) counts as either.
        if let Some(flat) = golden(&shot(&graph)) {
            let model: String = expected
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .flat_map(|line| line.chars().chain(['\n']))
                .collect();
            assert!(
                flat.chars()
                    .zip(model.chars())
                    .all(|(f, m)| f == m || f == '?')
                    && flat.len() == model.len(),
                "a flat item in the same pose covers the same cells\nflat:\n{flat}"
            );
        }
    }

    /// ADR-0107 decision 6: a model's source's video filters run over the layer each
    /// item draws the model into, before the item's effects, so a filtered model is
    /// drawn and filtered where it used to draw nothing (#1661). Each item that shows
    /// it has a chain of its own.
    #[test]
    fn golden_a_models_source_filters_run_over_its_layer() {
        let blue = Edit::FilterAdd {
            owner: FilterOwner::Source {
                source_id: SourceId::new("cam"),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new("blue"),
            kind: "color_correction".into(),
            settings: [("multiply", serde_json::json!("#0000ff"))]
                .into_iter()
                .collect(),
            enabled: Some(true),
            index: None,
        };
        let mut graph = placed(&[("cam", (16.0, 18.0), (32.0, 36.0), 0.0)]);
        apply(&mut graph, vec![hide("logo"), blue]);
        let filtered = "
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            BBBBBBBB........
            ";
        assert_golden(
            &model_shot(&graph, "plane"),
            filtered,
            "the source's filter colours the model",
        );
        // The item's effects read what the source's filters drew. The same filter ID
        // on both is two filters.
        apply(
            &mut graph,
            vec![Edit::FilterAdd {
                owner: FilterOwner::Item {
                    item_id: ItemId::new("cam"),
                },
                chain: FilterChain::Video,
                filter_id: FilterId::new("blue"),
                kind: "color_correction".into(),
                settings: [("multiply", serde_json::json!("#ffffff"))]
                    .into_iter()
                    .collect(),
                enabled: Some(true),
                index: None,
            }],
        );
        let shot = model_shot(&graph, "plane");
        assert_golden(&shot, filtered, "then the item's effects run over that");
        // A second item of the model is filtered by a chain of its own.
        let mut right = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(56.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(16.0, 36.0).expect("positive"),
        )
        .expect("valid");
        right.fit_mode = jstream_protocol::FitMode::Stretch;
        apply(
            &mut graph,
            vec![Edit::ItemAdd {
                scene_id: SceneId::new("main"),
                item_id: ItemId::new("again"),
                source_id: SourceId::new("cam"),
                transform: None,
                geometry: Some(right),
                opacity: None,
                blend: None,
            }],
        );
        let shot = model_shot(&graph, "plane");
        assert_golden(
            &shot,
            "
            BBBBBBBB....BBBB
            BBBBBBBB....BBBB
            BBBBBBBB....BBBB
            BBBBBBBB....BBBB
            BBBBBBBB....BBBB
            BBBBBBBB....BBBB
            BBBBBBBB....BBBB
            BBBBBBBB....BBBB
            BBBBBBBB....BBBB
            ",
            "each item of the model is filtered",
        );
    }

    /// `cam`, cropped to the middle half of its frame and stretched over the canvas.
    /// A vignette on the item frames the cropped shot. The same vignette on the source
    /// frames the source's whole frame, so its corners are cut away with the rest
    /// (issue #529's case).
    #[test]
    fn golden_a_vignette_on_a_cropped_item() {
        let mut graph = placed(&[("cam", (32.0, 18.0), (64.0, 36.0), 0.0)]);
        let crop = Edit::ItemSetCrop {
            item_id: ItemId::new("cam"),
            scene_id: None,
            crop: jstream_protocol::Crop {
                left: 4,
                top: 0,
                right: 4,
                bottom: 0,
            },
        };
        apply(&mut graph, vec![hide("logo"), crop]);
        let mut on_source = graph.clone();
        apply(
            &mut graph,
            vec![vignette(FilterOwner::Item {
                item_id: ItemId::new("cam"),
            })],
        );
        apply(
            &mut on_source,
            vec![vignette(FilterOwner::Source {
                source_id: SourceId::new("cam"),
            })],
        );
        assert_golden(
            &shot(&graph),
            "
            BBWWWWWWWWWWWWBB
            WWWWWWWWWWWWWWWB
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            BWWWWWWWWWWWWWWB
            BBBWWWWWWWWWWBBB
            ",
            "on the item, the vignette frames the cropped shot",
        );
        let on_source = shot(&on_source);
        assert!(
            on_source.chain(&SourceId::new("cam")).is_some(),
            "the source's chain runs"
        );
        assert_golden(
            &on_source,
            "
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            ",
            "on the source, the crop cuts its corners away",
        );
    }

    /// `logo` on the canvas's left half and `cam` on its right. A scene filter frames
    /// the scene's whole picture, and the same vignette as each item's effect frames
    /// each box.
    #[test]
    fn golden_a_vignette_on_a_scene() {
        let mut graph = placed(&[
            ("logo", (16.0, 18.0), (32.0, 36.0), 0.0),
            ("cam", (48.0, 18.0), (32.0, 36.0), 0.0),
        ]);
        let mut on_items = graph.clone();
        apply(
            &mut graph,
            vec![vignette(FilterOwner::Scene {
                scene_id: SceneId::new("main"),
            })],
        );
        apply(
            &mut on_items,
            ["logo", "cam"]
                .into_iter()
                .map(|id| {
                    vignette(FilterOwner::Item {
                        item_id: ItemId::new(id),
                    })
                })
                .collect(),
        );
        assert_golden(
            &shot(&graph),
            "
            BBWWWWWWWWWWWWBB
            WWWWWWWWWWWWWWWB
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            BWWWWWWWWWWWWWWB
            BBBWWWWWWWWWWBBB
            ",
            "on the scene, one ellipse frames the canvas",
        );
        assert_golden(
            &shot(&on_items),
            "
            BWWWWWWBBWWWWWWB
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWBWWWWWWWB
            BWWWWWWBBWWWWWWB
            ",
            "on each item, an ellipse frames each box",
        );
    }

    /// `cam` in a 32x16 box turned 90 degrees, so it stands 16 wide and 32 tall. Its
    /// vignette is drawn over the flat box and turned with it, so the ellipse is the
    /// box's, standing.
    #[test]
    fn golden_a_vignette_on_a_rotated_item() {
        let mut graph = placed(&[("cam", (32.0, 20.0), (32.0, 16.0), 90.0)]);
        apply(
            &mut graph,
            vec![
                hide("logo"),
                vignette(FilterOwner::Item {
                    item_id: ItemId::new("cam"),
                }),
            ],
        );
        assert_golden(
            &shot(&graph),
            "
            ................
            ......WWWB......
            ......WWWW......
            ......WWWW......
            ......WWWW......
            ......WWWW......
            ......WWWW......
            ......WWWW......
            ......BWWB......
            ",
            "the ellipse turns with the box",
        );
    }

    /// `effect` of `kind` with `settings` on item `id`'s effects.
    fn effect(id: &str, kind: &str, settings: serde_json::Value) -> Edit {
        Edit::FilterAdd {
            owner: FilterOwner::Item {
                item_id: ItemId::new(id),
            },
            chain: FilterChain::Video,
            filter_id: FilterId::new(kind),
            kind: kind.into(),
            settings: serde_json::from_value(settings).expect("an object"),
            enabled: Some(true),
            index: None,
        }
    }

    /// `cam`, opaque white, in a 24x12 box in the middle of the canvas, with a strong
    /// blue glow or a hard blue drop shadow on the item. The glow's samples 2 px past
    /// the box are a partly lit blue over black, so they read `?`; cut at the box, as
    /// before, they read `.`. The effects layer has room past
    /// the box (the kinds' `max_outset_px`, ADR-0065 §5), so the light and the shadow of
    /// an opaque picture show around it instead of being cut at its edge (#1226).
    #[test]
    fn golden_a_glow_and_a_shadow_show_past_an_opaque_items_box() {
        let graph = placed(&[("cam", (32.0, 18.0), (24.0, 12.0), 0.0)]);
        let mut glow = graph.clone();
        apply(
            &mut glow,
            vec![
                hide("logo"),
                effect(
                    "cam",
                    "glow",
                    serde_json::json!({"color": "#0000ff", "radius": 4.0, "strength": 4.0}),
                ),
            ],
        );
        assert_golden(
            &shot(&glow),
            "
            ................
            ................
            ....????????....
            ....?WWWWWW?....
            ....?WWWWWW?....
            ....?WWWWWW?....
            ....????????....
            ................
            ................
            ",
            "the glow rings the box",
        );
        let mut shadow = graph;
        apply(
            &mut shadow,
            vec![
                hide("logo"),
                effect(
                    "cam",
                    "drop_shadow",
                    serde_json::json!({
                        "color": "#0000ff", "offset_x": 8.0, "offset_y": 8.0, "blur": 0.0
                    }),
                ),
            ],
        );
        assert_golden(
            &shot(&shadow),
            "
            ................
            ................
            ................
            .....WWWWWW.....
            .....WWWWWW.....
            .....WWWWWWBB...
            .......BBBBBB...
            .......BBBBBB...
            ................
            ",
            "the shadow falls past the box, below and to the right",
        );
    }

    /// A pad grows a filter's output extent by as many extent pixels as its texels stand
    /// for, so the texels per extent pixel the next filter scales by hold (#1720); over
    /// an input at its own size it grows both alike, as before.
    #[test]
    fn a_pad_grows_the_extent_in_its_own_pixels() {
        // Half as many texels as its extent: 8 texels of pad are 16 extent pixels.
        assert_eq!(
            super::placed_after((16_400, 28), (16_384, 12), ((8192, 6), (8208, 22)), 8),
            (16_416, 44)
        );
        // At its own size, as `output_size` grows it.
        assert_eq!(
            super::placed_after((80, 52), (64, 36), ((64, 36), (80, 52)), 8),
            (80, 52)
        );
        // No pad: whatever the filter sizes it to.
        assert_eq!(
            super::placed_after((10, 10), (16_384, 12), ((8192, 6), (10, 10)), 0),
            (10, 10)
        );
    }

    /// An item wider than the largest layer side is drawn into a layer scaled down to
    /// fit, placed at the box's size in canvas pixels (ADR-0219, #1720): its drop
    /// shadow falls as far across the canvas as a small item's, and isn't cut by the
    /// pad. Before, the layer was placed at its own size, so the shadow's 8 px counted
    /// in layer pixels and fell 16 canvas px away; and the box and pad, each rounded up,
    /// made the padded layer one pixel too wide for one texture, so nothing was drawn.
    #[test]
    fn golden_a_big_items_shadow_reaches_as_far_as_a_small_items() {
        let shadow = || {
            effect(
                "cam",
                "drop_shadow",
                serde_json::json!({
                    "color": "#0000ff", "offset_x": 8.0, "offset_y": 8.0, "blur": 0.0
                }),
            )
        };
        // Both boxes end at x 44 and span rows 12 to 24.
        let mut small = placed(&[("cam", (26.0, 18.0), (36.0, 12.0), 0.0)]);
        apply(&mut small, vec![hide("logo"), shadow()]);
        assert_golden(
            &shot(&small),
            "
            ................
            ................
            ................
            ..WWWWWWWWW.....
            ..WWWWWWWWW.....
            ..WWWWWWWWWBB...
            ....BBBBBBBBB...
            ....BBBBBBBBB...
            ................
            ",
            "a small item's shadow, 8 px right and down",
        );
        let mut big = placed(&[("cam", (44.0 - 8192.0, 18.0), (16_384.0, 12.0), 0.0)]);
        apply(&mut big, vec![hide("logo"), shadow()]);
        let shot = shot(&big);
        let Some((grid, layer)) = golden_at(&shot, 100) else {
            return;
        };
        let expected: String = "
            ................
            ................
            ................
            WWWWWWWWWWW.....
            WWWWWWWWWWW.....
            WWWWWWWWWWWBB...
            BBBBBBBBBBBBB...
            BBBBBBBBBBBBB...
            ................
            "
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .flat_map(|line| line.chars().chain(['\n']))
        .collect();
        assert_eq!(
            grid, expected,
            "the shadow falls 8 px right and down, as the small item's, uncut\n{grid}"
        );
        let [texels, extent] = layer.expect("cam's layer");
        assert!(texels.0 <= 8192, "scaled down to fit: {texels:?}");
        assert_eq!(extent, (16_384, 12), "placed at the box's size");
    }

    /// An item's layer grows by what its effects draw at their settings, not the most
    /// their kinds could (ADR-0136): a drop shadow and an outline at their
    /// defaults (8 px offset and blur, a 4 px stroke) pad it by 16 px each, not 300 and
    /// 65.
    #[test]
    fn effects_at_their_defaults_pad_by_what_they_draw() {
        let mut graph = placed(&[("cam", (32.0, 18.0), (64.0, 36.0), 0.0)]);
        apply(
            &mut graph,
            vec![
                hide("logo"),
                effect("cam", "drop_shadow", serde_json::json!({})),
                effect("cam", "outline", serde_json::json!({})),
            ],
        );
        let shot = shot(&graph);
        let chain = shot.item_chain(&SceneId::new("main"), &ItemId::new("cam"));
        assert_eq!(chain.map(|chain| chain.outset_also(&[])), Some(32));
        assert_eq!(chain.and_then(|c| c.pad), Some((0, 32)));
    }

    /// A border with no outer stroke draws nothing past the box, so it doesn't grow the
    /// layer; with one, the layer grows by the stroke after its gap (#1376).
    #[test]
    fn a_border_pads_only_for_its_outer_stroke() {
        let pad = |settings: serde_json::Value| {
            let mut graph = placed(&[("cam", (32.0, 18.0), (64.0, 36.0), 0.0)]);
            apply(
                &mut graph,
                vec![hide("logo"), effect("cam", "border", settings)],
            );
            let shot = shot(&graph);
            let chain = shot.item_chain(&SceneId::new("main"), &ItemId::new("cam"));
            chain.and_then(|c| c.pad)
        };
        assert_eq!(pad(serde_json::json!({"width": 4.0})), None);
        // 20 + 10 + 1 px, rounded up to the 16 px step.
        assert_eq!(
            pad(serde_json::json!({"outer_width": 10.0, "outer_gap": 20.0})),
            Some((0, 32))
        );
    }

    /// A `border` with rounded corners and an outer stroke on an opaque item (#1227): the
    /// stroke rings the box past its edge, in the room the border's own outset gives it,
    /// and the corners of the picture are cut round. The `?` samples fall on the
    /// antialiased curve of a corner; the grid's samples sit right of and below each
    /// cell's centre, so the right and bottom corners show more of them.
    #[test]
    fn golden_a_border_rings_an_opaque_items_box() {
        let mut graph = placed(&[("cam", (32.0, 18.0), (24.0, 12.0), 0.0)]);
        apply(
            &mut graph,
            vec![
                hide("logo"),
                effect(
                    "cam",
                    "border",
                    serde_json::json!({
                        "width": 0.0, "radius": 6.0, "outer_width": 3.0,
                        "outer_color": "#0000ff"
                    }),
                ),
            ],
        );
        assert_golden(
            &shot(&graph),
            "
            ................
            ................
            .....BBBBB?.....
            ....BWWWWW??....
            ....BWWWWWWB....
            ....??WWWW?.....
            .....?BBBB......
            ................
            ................
            ",
            "the outer stroke rings the box",
        );
    }

    /// A vignette before an effect that draws past the box (a glow, here an invisible
    /// one) frames the box itself, as it does alone: the layer grows only where the glow
    /// runs (`Chain::pad_at`), so rounded corners or a vignette followed by a shadow keep
    /// their shape (#1226).
    #[test]
    fn golden_effects_before_a_glow_frame_the_box_itself() {
        let mut graph = placed(&[("cam", (32.0, 18.0), (64.0, 36.0), 0.0)]);
        apply(
            &mut graph,
            vec![
                hide("logo"),
                vignette(FilterOwner::Item {
                    item_id: ItemId::new("cam"),
                }),
                effect(
                    "cam",
                    "glow",
                    serde_json::json!({"color": "#0000ff00", "radius": 50.0}),
                ),
            ],
        );
        let shot = shot(&graph);
        let chain = shot.item_chain(&SceneId::new("main"), &ItemId::new("cam"));
        let chain = chain.expect("cam's effects");
        // The glow's radius, 50, rounded up to the 16 px step (ADR-0136).
        assert_eq!(chain.pad, Some((1, 64)));
        // The pad copy is one more layer and pass, the glow's input grown by the pad
        // on every side (#1285).
        let box_size = (64, 36);
        let sizes = chain.layer_sizes(box_size.0, box_size.1);
        assert_eq!(chain.pad_layer(box_size.0, box_size.1), Some((192, 164)));
        let bytes = |(w, h): (u32, u32)| u64::from(w) * u64::from(h) * 8;
        let cost = shot.chain_cost([]);
        assert_eq!(
            (cost.item_layers, cost.item_passes),
            (4, 4),
            "the box, the vignette, the pad copy and the glow"
        );
        assert_eq!(
            cost.layer_bytes,
            bytes(box_size) + sizes.iter().copied().map(bytes).sum::<u64>() + bytes((192, 164))
        );
        assert_golden(
            &shot,
            "
            BBWWWWWWWWWWWWBB
            WWWWWWWWWWWWWWWB
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            WWWWWWWWWWWWWWWW
            BWWWWWWWWWWWWWWB
            BBBWWWWWWWWWWBBB
            ",
            "the vignette frames the box, not the glow's room",
        );
    }

    /// `cam` in the left half of the canvas, flipped upside down, with a vignette on the
    /// item. In box space the vignette is the box's ellipse; with its effects in canvas
    /// space (ADR-0106) it is drawn over the placed item, so it is the canvas's ellipse,
    /// whatever the item's place and flip, and the right half stays clear.
    #[test]
    fn golden_a_vignette_in_canvas_space_is_the_canvas() {
        let mut graph = placed(&[("cam", (16.0, 18.0), (32.0, 36.0), 0.0)]);
        let mut geometry = jstream_protocol::ItemGeometry::try_new(
            jstream_protocol::Vec2::new(16.0, 18.0).expect("finite"),
            jstream_protocol::Size::try_new(32.0, 36.0).expect("positive"),
        )
        .expect("valid");
        geometry.fit_mode = jstream_protocol::FitMode::Stretch;
        geometry.flip_v = true;
        apply(
            &mut graph,
            vec![
                hide("logo"),
                Edit::ItemSetGeometry {
                    scene_id: None,
                    item_id: ItemId::new("cam"),
                    geometry,
                },
                vignette(FilterOwner::Item {
                    item_id: ItemId::new("cam"),
                }),
            ],
        );
        assert_golden(
            &shot(&graph),
            "
            BWWWWWWB........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWB........
            BWWWWWWB........
            ",
            "in box space the ellipse is the box's",
        );
        apply(
            &mut graph,
            vec![Edit::ItemSetEffectsSpace {
                item_id: ItemId::new("cam"),
                space: jstream_protocol::EffectsSpace::Canvas,
            }],
        );
        assert_golden(
            &shot(&graph),
            "
            BBWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            WWWWWWWW........
            BWWWWWWW........
            BBBWWWWW........
            ",
            "in canvas space the ellipse is the canvas's, cut to the item",
        );
    }

    /// A `color_correction` filter `id` multiplying by `colour` on `owner`'s video chain.
    fn multiply(owner: FilterOwner, id: &str, colour: &str) -> Edit {
        Edit::FilterAdd {
            owner,
            chain: FilterChain::Video,
            filter_id: FilterId::new(id),
            kind: "color_correction".into(),
            settings: [("multiply", serde_json::json!(colour))]
                .into_iter()
                .collect(),
            enabled: None,
            index: None,
        }
    }

    fn record(id: &str) -> Edit {
        Edit::OutputCreate {
            output_id: OutputId::new(id),
            kind: OutputKind::Record {
                path: format!("/home/me/{id}.mp4"),
                container: RecordContainer::Mp4,
            },
        }
    }

    fn output(id: &str) -> FilterOwner {
        FilterOwner::Output {
            output_id: OutputId::new(id),
        }
    }

    /// The master stage's video chain is planned over the program and each video mix,
    /// and an output's own over what the master stage makes of the mix it shows, or
    /// over the mix itself without one (ADR-0080). A disabled filter plans nothing.
    #[test]
    fn the_output_stage_chains_are_planned_per_mix_and_output() {
        let mut g = graph(&[]);
        apply(
            &mut g,
            vec![
                record("rec"),
                record("vod"),
                multiply(output("rec"), "o", "#ff0000"),
            ],
        );
        let plain = shot(&g);
        assert_eq!(plain.stage_chain(&ChainOwner::Master(None)), None);
        assert_eq!(
            plain
                .stage_chain(&ChainOwner::Output(OutputId::new("rec")))
                .map(|chain| &chain.input),
            Some(&GpuKey::Mixed(None)),
            "no master chain: the output's reads the mix"
        );
        assert!(plain.changes_with_time());

        apply(
            &mut g,
            vec![
                Edit::VideoMixCreate {
                    mix_id: VideoMixId::new("vertical"),
                    name: "Vertical".into(),
                    mix: jstream_protocol::VideoMix {
                        input: jstream_protocol::MixInput::Program,
                        resolution: jstream_protocol::MixResolution {
                            width: 1080,
                            height: 1920,
                        },
                        fps_divisor: 1,
                    },
                },
                Edit::OutputSetVideoMix {
                    output_id: OutputId::new("rec"),
                    mix_id: Some(VideoMixId::new("vertical")),
                },
                multiply(FilterOwner::Master, "m", "#ff0000"),
                multiply(output("vod"), "o", "#00ff00"),
                Edit::FilterSetEnabled {
                    owner: output("vod"),
                    filter_id: FilterId::new("o"),
                    enabled: false,
                },
            ],
        );
        let staged = shot(&g);
        let vertical = Some(VideoMixId::new("vertical"));
        for mix in [None, vertical.clone()] {
            assert_eq!(
                staged
                    .stage_chain(&ChainOwner::Master(mix.clone()))
                    .map(|chain| &chain.input),
                Some(&GpuKey::Mixed(mix)),
            );
        }
        assert_eq!(
            staged
                .stage_chain(&ChainOwner::Output(OutputId::new("rec")))
                .map(|chain| &chain.input),
            Some(&GpuKey::Filtered(ChainOwner::Master(vertical))),
        );
        assert_eq!(
            staged.stage_chain(&ChainOwner::Output(OutputId::new("vod"))),
            None,
            "a disabled filter is bypassed"
        );
    }

    /// A LUT on the master stage grades every output's frame, and one on an output's
    /// own chain grades that output's after it, each once its table is loaded; the
    /// editing canvas shows the mix before them.
    #[test]
    fn the_output_stage_reads_its_filters_assets() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let lut: Settings = [(
            "lut",
            serde_json::json!({"type": "asset", "root": "app", "path": "invert.cube"}),
        )]
        .into_iter()
        .collect();
        let key = filter_assets::wanted("apply_lut", &lut).expect("a LUT");
        let invert = |owner: FilterOwner, id: &str| Edit::FilterAdd {
            owner,
            chain: FilterChain::Video,
            filter_id: FilterId::new(id),
            kind: "apply_lut".into(),
            settings: lut.clone(),
            enabled: None,
            index: None,
        };
        let mut g = graph(&[]);
        apply(
            &mut g,
            vec![
                record("rec"),
                invert(FilterOwner::Master, "m"),
                invert(output("rec"), "o"),
            ],
        );
        let rec = OutputId::new("rec");
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let mut views = |shot: &Shot| {
            let canvas = shot.canvas().expect("canvas");
            draw_views(
                &mut compositor,
                (&mut cache, &mut chains),
                shot,
                &[
                    (snapshot::View::default(), canvas),
                    (snapshot::View::output(None, None), canvas),
                    (snapshot::View::output(None, Some(&rec)), canvas),
                ],
            )
        };
        assert_eq!(
            views(&shot(&g)),
            [WHITE, WHITE, WHITE],
            "no table yet: passed through"
        );
        let table = Arc::new(FilterAsset {
            version: 1,
            loaded: Loaded::Lut(Lut::parse_cube(INVERT).expect("cube")),
        });
        let black = [0, 0, 0, 255];
        assert_eq!(
            views(&shot_with(&g, &FilterAssets::from([(key, table)]))),
            [WHITE, black, WHITE],
            "the master inverts every output; rec's own inverts it back"
        );
    }

    /// A virtual camera always draws the program at the canvas size, so its own chain is
    /// planned over the program's master stage even when it names a video mix, and its
    /// frame runs it (#1006).
    #[test]
    fn a_virtual_camera_naming_a_mix_runs_its_own_chain_over_the_program() {
        let mut g = graph(&[]);
        let vertical = VideoMixId::new("vertical");
        let cam = OutputId::new("cam");
        apply(
            &mut g,
            vec![
                Edit::VideoMixCreate {
                    mix_id: vertical.clone(),
                    name: "Vertical".into(),
                    mix: jstream_protocol::VideoMix {
                        input: jstream_protocol::MixInput::Program,
                        resolution: jstream_protocol::MixResolution {
                            width: 1080,
                            height: 1920,
                        },
                        fps_divisor: 1,
                    },
                },
                Edit::OutputCreate {
                    output_id: cam.clone(),
                    kind: OutputKind::VirtualCam,
                },
                Edit::OutputSetVideoMix {
                    output_id: cam.clone(),
                    mix_id: Some(vertical),
                },
                multiply(output("cam"), "o", "#ff0000"),
            ],
        );
        let plain = shot(&g);
        assert_eq!(
            plain
                .stage_chain(&ChainOwner::Output(cam.clone()))
                .map(|chain| &chain.input),
            Some(&GpuKey::Mixed(None)),
            "the camera's chain reads the program, not the mix it names"
        );
        apply(&mut g, vec![multiply(FilterOwner::Master, "m", "#ff00ff")]);
        let staged = shot(&g);
        assert_eq!(
            staged
                .stage_chain(&ChainOwner::Output(cam.clone()))
                .map(|chain| &chain.input),
            Some(&GpuKey::Filtered(ChainOwner::Master(None))),
        );

        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let canvas = staged.canvas().expect("canvas");
        let pixels = draw_views(
            &mut compositor,
            (&mut cache, &mut chains),
            &staged,
            &[(snapshot::View::output(None, Some(&cam)), canvas)],
        );
        assert_eq!(
            pixels,
            [RED],
            "the master's and the camera's chains both ran"
        );
    }

    /// `stats.tick`'s chain cost counts the output stage for the running outputs only
    /// (ADR-0080): each staged mix's canvas-sized layer and its master chain once,
    /// however many outputs get it, and each output's own chain; an output with neither
    /// chain adds nothing.
    #[test]
    fn the_chain_cost_counts_the_running_outputs_stage() {
        use jstream_protocol::ChainCost;

        let rec = OutputId::new("rec");
        let mut g = graph(&[]);
        apply(
            &mut g,
            vec![
                record("rec"),
                record("vod"),
                multiply(output("rec"), "o", "#ff0000"),
            ],
        );
        let plain = shot(&g);
        let canvas = plain.canvas().expect("a canvas");
        let canvas_bytes = u64::from(canvas.width()) * u64::from(canvas.height()) * 8;
        assert_eq!(plain.chain_cost([]), ChainCost::default(), "nothing runs");
        assert_eq!(
            plain.chain_cost([(None, None)]),
            ChainCost::default(),
            "no master chain and none of its own: drawn straight to the output"
        );
        assert_eq!(
            plain.chain_cost([(None, None), (None, Some(&rec))]),
            ChainCost {
                stage_chains: 1,
                stage_layers: 2,
                stage_passes: 2,
                layer_bytes: 2 * canvas_bytes,
                ..ChainCost::default()
            },
            "the program's layer and rec's tint"
        );

        let vertical = VideoMixId::new("vertical");
        apply(
            &mut g,
            vec![
                Edit::VideoMixCreate {
                    mix_id: vertical.clone(),
                    name: "Vertical".into(),
                    mix: jstream_protocol::VideoMix {
                        input: jstream_protocol::MixInput::Program,
                        resolution: jstream_protocol::MixResolution {
                            width: 1080,
                            height: 1920,
                        },
                        fps_divisor: 1,
                    },
                },
                Edit::OutputSetVideoMix {
                    output_id: rec.clone(),
                    mix_id: Some(vertical.clone()),
                },
                multiply(FilterOwner::Master, "m", "#ff00ff"),
            ],
        );
        let staged = shot(&g);
        assert_eq!(
            staged.chain_cost([(None, None)]),
            ChainCost {
                stage_chains: 1,
                stage_layers: 2,
                stage_passes: 2,
                layer_bytes: 2 * canvas_bytes,
                ..ChainCost::default()
            },
            "the program's layer and the master chain; the vertical mix's isn't drawn"
        );
        assert_eq!(
            staged.chain_cost([(None, None), (None, Some(&rec))]),
            staged.chain_cost([(None, None)]),
            "rec drawn on the program shares its layer and master chain, and its own \
             chain, which follows the vertical mix, doesn't run there"
        );
        assert_eq!(
            staged.chain_cost([(None, None), (Some(&vertical), Some(&rec))]),
            ChainCost {
                stage_chains: 3,
                stage_layers: 5,
                stage_passes: 5,
                layer_bytes: 5 * canvas_bytes,
                ..ChainCost::default()
            },
            "each mix's layer and master chain, then rec's own"
        );
    }

    /// A pack filter drawn only on the master stage or only on an output's is still in
    /// `Shot::pack_kinds`, so a device loss while it draws blames it (#349, ADR-0080); a
    /// disabled one on either stage isn't.
    #[test]
    fn pack_kinds_count_the_master_and_output_stages() {
        use crate::effect_packs::test_pack::{self, KIND, Temp};

        let temp = Temp::new("stages");
        let assets = FilterAssets::new().with_effects(Arc::new(
            [(KIND.to_owned(), test_pack::effect(&temp, test_pack::MUL))]
                .into_iter()
                .collect(),
        ));
        let pack = |owner: FilterOwner, id: &str| Edit::FilterAdd {
            owner,
            chain: FilterChain::Video,
            filter_id: FilterId::new(id),
            kind: KIND.into(),
            settings: Settings::default(),
            enabled: None,
            index: None,
        };
        let enable = |owner: FilterOwner, id: &str, enabled: bool| Edit::FilterSetEnabled {
            owner,
            filter_id: FilterId::new(id),
            enabled,
        };
        let mut g = graph(&[("cam", true)]).with_kinds([test_pack::descriptor()]);
        apply(
            &mut g,
            vec![
                record("rec"),
                pack(FilterOwner::Master, "m"),
                pack(output("rec"), "o"),
                enable(output("rec"), "o", false),
            ],
        );
        let kinds =
            |g: &Graph| -> Vec<String> { shot_with(g, &assets).pack_kinds().into_iter().collect() };
        // The source's chain is built in, so only the master stage counts here.
        assert_eq!(kinds(&g), [KIND], "the master stage's filter");

        apply(
            &mut g,
            vec![
                enable(FilterOwner::Master, "m", false),
                enable(output("rec"), "o", true),
            ],
        );
        let shot = shot_with(&g, &assets);
        assert_eq!(shot.stage_chain(&ChainOwner::Master(None)), None);
        assert_eq!(kinds(&g), [KIND], "the output stage's filter");

        apply(&mut g, vec![enable(output("rec"), "o", false)]);
        assert!(kinds(&g).is_empty(), "disabled on both stages");
    }

    /// Draws each of `views` of `shot` at its size in one cache frame, as the render
    /// thread draws a tick's frames, and reads back each one's top-left pixel.
    fn draw_views(
        compositor: &mut Compositor,
        (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
        shot: &Shot,
        views: &[(snapshot::View<'_>, FrameSize)],
    ) -> Vec<[u8; 4]> {
        cache.begin_frame();
        let at = MediaTime::zero(Timebase::MILLIS);
        let pixels = views
            .iter()
            .map(|(view, size)| {
                let output = snapshot::compose(
                    compositor,
                    (&mut *cache, &mut *chains),
                    (shot, at),
                    *view,
                    &LiveParams::default(),
                    &NoCaptures,
                    snapshot::DrawAt::read(*size),
                )
                .expect("compose");
                output
                    .pixels()
                    .expect("read back")
                    .pixel(1, 1)
                    .expect("a pixel")
            })
            .collect();
        drop(cache.end_frame(compositor));
        pixels
    }

    /// The program scene's background is in the mix the output stage filters (ADR-0089
    /// §4): an output with a master chain draws the background through it, where nothing
    /// covers it, and one with none draws it as it is.
    #[test]
    fn the_output_stage_filters_the_programs_background() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let mut g = Graph::new().with_kinds(crate::kinds::builtin());
        apply(
            &mut g,
            vec![
                Edit::SceneCreate {
                    scene_id: SceneId::new("main"),
                    name: "Main".into(),
                    role: None,
                },
                Edit::SceneSetOptions {
                    scene_id: SceneId::new("main"),
                    options: jstream_protocol::SceneOptions {
                        background: Some("#ff00ff".into()),
                        ..jstream_protocol::SceneOptions::default()
                    },
                },
                record("rec"),
            ],
        );
        let magenta = [255, 0, 255, 255];
        let plain = shot(&g);
        let canvas = plain.canvas().expect("canvas");
        let pixels = draw_views(
            &mut compositor,
            (&mut cache, &mut chains),
            &plain,
            &[(
                snapshot::View::output(None, Some(&OutputId::new("rec"))),
                canvas,
            )],
        );
        assert_eq!(pixels, [magenta], "no chain: the background as it is");

        apply(&mut g, vec![multiply(FilterOwner::Master, "m", "#0000ff")]);
        let staged = shot(&g);
        let pixels = draw_views(
            &mut compositor,
            (&mut cache, &mut chains),
            &staged,
            &[
                (snapshot::View::default(), canvas),
                (
                    snapshot::View::output(None, Some(&OutputId::new("rec"))),
                    canvas,
                ),
            ],
        );
        assert_eq!(
            pixels,
            [magenta, [0, 0, 255, 255]],
            "the editing canvas shows the background, the output it filtered"
        );
    }

    /// The master stage's chain shows on every output's frame and not before it (the
    /// editing canvas, a preview); an output's own shows on its frame alone, after the
    /// master's. Two outputs drawing the program at different sizes composite it and run
    /// the master chain once.
    #[test]
    fn the_output_stage_runs_on_outputs_only() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let mut g = graph(&[]);
        apply(
            &mut g,
            vec![
                record("rec"),
                multiply(FilterOwner::Master, "m", "#ff00ff"),
                multiply(output("rec"), "o", "#ff0000"),
            ],
        );
        let staged = shot(&g);
        let canvas = staged.canvas().expect("canvas");
        let half = FrameSize::new(canvas.width() / 2, canvas.height() / 2).expect("half");
        let rec = OutputId::new("rec");
        let pixels = draw_views(
            &mut compositor,
            (&mut cache, &mut chains),
            &staged,
            &[
                (snapshot::View::default(), canvas),
                (snapshot::View::output(None, None), canvas),
                (snapshot::View::output(None, None), half),
                (snapshot::View::output(None, Some(&rec)), half),
            ],
        );
        let magenta = [255, 0, 255, 255];
        assert_eq!(pixels, [WHITE, magenta, magenta, RED]);
        assert_eq!(
            chains.staged.len(),
            3,
            "the program's layer, its master chain and the output's chain, once each"
        );
        assert_eq!(chains.len(), 2);

        // Without the chains the filters are retired and the output draws the mix.
        let plain = shot(&graph(&[]));
        let pixels = draw_views(
            &mut compositor,
            (&mut cache, &mut chains),
            &plain,
            &[(snapshot::View::output(None, Some(&rec)), canvas)],
        );
        assert_eq!(pixels, [WHITE]);
        assert_eq!(chains.len(), 0);
    }
}
