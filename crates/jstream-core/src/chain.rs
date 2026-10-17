//! Video filter chains (ADR-0026, roadmap V0): a source's enabled video filters, run in
//! order over its frame before it is placed, with `jstream-render`'s built-in filter
//! kinds.
//!
//! [`plan`](crate::snapshot::plan_with) copies each reached source's chain into the
//! [`Shot`](crate::snapshot::Shot) as a [`Chain`]: its input (the source's frame upload,
//! or the display capture) and the filters that run, each with its settings. A layer
//! that places the source then shows the chain's output instead of the input.
//!
//! Each compositor that draws shots keeps a [`Chains`]: one [`VideoFilter`] per filter,
//! made when the filter first draws and kept while the shot keeps it, so a stateful kind
//! (a video delay) sees every frame in order, and a settings change updates it in place.
//! A kind that reads an asset is made again, with the new image, when a new load of
//! that asset arrives.
//! Each filter draws into a layer of the compositor's [`ResourceCache`], under
//! [`GpuKey::Chain`], with the last one under [`GpuKey::Filtered`], so a steady chain
//! allocates no output texture. A filter that no shot keeps is retired into the cache
//! ([`ResourceCache::retire_filter`]), which hands it back with the images it reclaims
//! once the GPU is done with it, so the render thread drops it on its collector
//! (R-RT-003).
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
use std::sync::Arc;

use jstream_graph::Source;
use jstream_media::MediaTime;
use jstream_protocol::{FilterChain, FilterId, Settings, SourceId};
use jstream_render::{Compositor, FilterClock, RenderError, ResourceCache, VideoFilter};

use crate::filter_assets::{self, FilterAsset, FilterAssets};
use crate::snapshot::GpuKey;

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
}

/// A source's video chain as a shot draws it: the image it starts from and its enabled
/// filters of a kind this engine draws, first filter first. Never empty.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Chain {
    pub(crate) input: GpuKey,
    pub(crate) filters: Vec<ChainFilter>,
}

impl Chain {
    /// `source`'s chain over `input`, or `None` when no filter of it runs: the chain is
    /// empty, every filter is disabled, or none is of a kind this engine draws (an
    /// effect pack that isn't loaded). Such a filter is bypassed. A filter that reads an
    /// asset gets it from `assets`.
    pub(crate) fn of(source: &Source, input: GpuKey, assets: &FilterAssets) -> Option<Self> {
        let filters: Vec<ChainFilter> = source
            .filters()
            .chain(FilterChain::Video)
            .iter()
            .filter(|filter| filter.enabled())
            .filter(|filter| {
                let drawn = VideoFilter::new(filter.kind(), filter.settings()).is_some();
                if !drawn {
                    tracing::debug!(
                        source = %source.id(),
                        filter = %filter.id(),
                        kind = filter.kind(),
                        "video filter kind is not drawn by this engine; bypassed"
                    );
                }
                drawn
            })
            .map(|filter| ChainFilter {
                id: filter.id().clone(),
                kind: filter.kind().to_owned(),
                settings: filter.settings().clone(),
                reference: filter_assets::wanted(filter.kind(), filter.settings())
                    .and_then(|key| assets.get(&key).cloned()),
            })
            .collect();
        (!filters.is_empty()).then_some(Self { input, filters })
    }

    /// The size the chain draws for an input of `width` by `height`: the source's
    /// reported size once filtered (aspect ratio, padding and the bounds-growing kinds
    /// change it).
    pub(crate) fn output_size(&self, width: u32, height: u32) -> (u32, u32) {
        self.filters
            .iter()
            .filter_map(|filter| VideoFilter::new(&filter.kind, &filter.settings))
            .fold((width, height), |(w, h), filter| filter.output_size(w, h))
    }
}

/// A filter as one compositor runs it.
struct Running {
    id: FilterId,
    kind: String,
    settings: Settings,
    filter: VideoFilter,
    /// The version of the asset whose image `filter` reads, if any.
    reference: Option<u64>,
    /// The layer it draws into ([`output_key`]), made when the chain's shape changes so
    /// a steady frame builds no key.
    key: GpuKey,
    /// Whether a failure to make its layer or draw has been logged, so a filter that
    /// keeps failing logs once; a frame it draws clears it, so a later failure logs
    /// again.
    warned: bool,
}

/// The filters one compositor runs, by source, kept from frame to frame.
#[derive(Default)]
pub(crate) struct Chains {
    running: BTreeMap<SourceId, Vec<Running>>,
    /// The sources whose chain failed to draw this frame, which draw nothing.
    failed: BTreeSet<SourceId>,
    /// Filters no shot runs any more, on their way to the cache.
    retired: Vec<VideoFilter>,
    /// The frame of the cache the chains last ran in. Every mix a tick draws shares one
    /// frame, and the chains run once in it, so a stateful filter sees each tick once.
    ran: Option<u64>,
    /// How many frames the chains ran in, for tests.
    #[cfg(test)]
    runs: usize,
}

impl Chains {
    /// Runs every chain in `chains` at `at`, between the caller's
    /// [`ResourceCache::begin_frame`] and [`ResourceCache::end_frame`], after the
    /// chains' inputs are in `cache`. A chain whose input is not cached (a failed or
    /// skipped upload) draws nothing.
    pub(crate) fn run(
        &mut self,
        compositor: &mut Compositor,
        cache: &mut ResourceCache<GpuKey>,
        chains: &BTreeMap<SourceId, Chain>,
        at: MediaTime,
    ) {
        let frame = cache.frame();
        if self.ran == Some(frame) {
            return;
        }
        self.ran = Some(frame);
        #[cfg(test)]
        {
            self.runs += 1;
        }
        self.failed.clear();
        let retired = &mut self.retired;
        self.running.retain(|source, running| {
            let keep = chains.contains_key(source);
            if !keep {
                retired.extend(running.drain(..).map(|r| r.filter));
            }
            keep
        });
        for (source, chain) in chains {
            if !self.running.contains_key(source) {
                self.running.insert(source.clone(), Vec::new());
            }
            if let Some(running) = self.running.get_mut(source) {
                sync(source, running, &chain.filters, &mut self.retired);
                for (r, f) in running.iter_mut().zip(&chain.filters) {
                    refer(compositor, source, r, f, &mut self.retired);
                }
            }
        }
        for filter in self.retired.drain(..) {
            cache.retire_filter(filter);
        }
        // Every output layer first, so the draws below read and write the cache
        // without borrowing it mutably.
        for (source, chain) in chains {
            let Some(running) = self.running.get_mut(source) else {
                continue;
            };
            let Some(input) = cache.get(&chain.input) else {
                self.failed.insert(source.clone());
                continue;
            };
            let mut size = (input.width(), input.height());
            for filter in running.iter_mut() {
                size = filter.filter.output_size(size.0, size.1);
                if let Err(error) = cache.layer(compositor, &filter.key, size.0, size.1) {
                    if !filter.warned {
                        filter.warned = true;
                        tracing::warn!(
                            source = %source,
                            filter = %filter.id,
                            kind = %filter.kind,
                            %error,
                            "video filter output can't be made; the source draws nothing"
                        );
                    }
                    self.failed.insert(source.clone());
                    break;
                }
            }
        }
        let clock = FilterClock {
            seconds: at.as_secs_f64(),
        };
        let cache = &*cache;
        for (source, chain) in chains {
            if self.failed.contains(source) {
                continue;
            }
            let Some(running) = self.running.get_mut(source) else {
                continue;
            };
            let mut input = &chain.input;
            for running in running.iter_mut() {
                let Running {
                    id,
                    kind,
                    filter,
                    key,
                    warned,
                    ..
                } = running;
                let drawn = match (cache.get(input), cache.get(key)) {
                    (Some(from), Some(to)) => compositor.apply_filter_into(filter, from, to, clock),
                    _ => Err(RenderError::Invalid {
                        what: "cache",
                        detail: format!("no image for {input:?} or {key:?}"),
                    }),
                };
                if let Err(error) = drawn {
                    if !*warned {
                        *warned = true;
                        tracing::warn!(
                            source = %source,
                            filter = %id,
                            kind = %kind,
                            %error,
                            "video filter failed to draw; the source draws nothing"
                        );
                    }
                    self.failed.insert(source.clone());
                    break;
                }
                *warned = false;
                input = key;
            }
        }
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

    /// Whether `key`, an image a layer shows, is a chain output that failed this frame.
    pub(crate) fn failed(&self, key: &GpuKey) -> bool {
        matches!(key, GpuKey::Filtered(source) if self.failed.contains(source))
    }

    /// How many filters run, for tests.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.running.values().map(Vec::len).sum()
    }
}

/// Where the filter at index `at` of `source`'s chain draws: the last one into the
/// chain's output, the others into their own layer.
fn output_key(source: &SourceId, at: usize, last: usize) -> GpuKey {
    if at == last {
        GpuKey::Filtered(source.clone())
    } else {
        GpuKey::Chain(source.clone(), at)
    }
}

/// Makes `running` run `filters`: a filter with the same ID and kind as the one at its
/// place keeps its state and takes new settings in place; any other change rebuilds the
/// list, keeping each filter whose ID and kind it finds, and gives each its layer's key
/// for `source`'s chain. Allocates nothing when nothing changed.
fn sync(
    source: &SourceId,
    running: &mut Vec<Running>,
    filters: &[ChainFilter],
    retired: &mut Vec<VideoFilter>,
) {
    let same_shape = running.len() == filters.len()
        && running
            .iter()
            .zip(filters)
            .all(|(r, f)| r.id == f.id && r.kind == f.kind);
    if same_shape {
        for (r, f) in running.iter_mut().zip(filters) {
            if r.settings != f.settings {
                r.filter.set_settings(&f.settings);
                r.settings = f.settings.clone();
            }
        }
        return;
    }
    let mut old: Vec<Running> = std::mem::take(running);
    for f in filters {
        let kept = old
            .iter()
            .position(|r| r.id == f.id && r.kind == f.kind)
            .map(|at| old.swap_remove(at));
        let entry = match kept {
            Some(mut r) => {
                if r.settings != f.settings {
                    r.filter.set_settings(&f.settings);
                    r.settings = f.settings.clone();
                }
                r
            }
            None => match VideoFilter::new(&f.kind, &f.settings) {
                Some(filter) => Running {
                    id: f.id.clone(),
                    kind: f.kind.clone(),
                    settings: f.settings.clone(),
                    filter,
                    reference: None,
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

/// Gives `running` the image of the asset `filter` reads, when that asset changed: the
/// compositor makes the image once per load. A kind that reads a reference keeps no
/// state between frames, so it is made again with the new image, and the old one, which
/// holds the old image, is retired with the cache's images (R-RT-003). An image that
/// can't be made is logged, and the filter passes its input through until the asset
/// changes.
fn refer(
    compositor: &mut Compositor,
    source: &SourceId,
    running: &mut Running,
    filter: &ChainFilter,
    retired: &mut Vec<VideoFilter>,
) {
    let wanted = filter.reference.as_ref().map(|asset| asset.version);
    if running.reference == wanted {
        return;
    }
    running.reference = wanted;
    let Some(mut fresh) = VideoFilter::new(&filter.kind, &filter.settings) else {
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

#[cfg(test)]
mod tests {
    use jstream_graph::Graph;
    use jstream_media::{MediaTime, Timebase};
    use jstream_protocol::{
        Edit, FilterChain, FilterId, FilterOwner, ItemId, SceneId, Settings, SourceId, SourceKind,
    };
    use std::sync::Arc;

    use jstream_render::{Compositor, Lut, ResourceCache};
    use jstream_sources::StillImage;

    use super::Chains;
    use crate::filter_assets::{self, FilterAsset, FilterAssets, Loaded};
    use crate::snapshot::tests::gpu;
    use crate::snapshot::{self, Frame, GpuKey, LiveParams, Look, NoCaptures, Shot};

    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];

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
            &mut |_| Some(Look::Frame(frame.clone(), None)),
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
                None,
                &LiveParams::default(),
                &NoCaptures,
                canvas,
            )
            .expect("compose");
            pixel = output.pixels().expect("read back").pixel(1, 1);
        }
        drop(cache.end_frame(compositor));
        pixel.expect("a pixel")
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

    /// A frame that only claims layers (a mix not due on that tick) keeps the chain's
    /// output layer, so the next frame that draws reuses it instead of reallocating.
    #[test]
    fn a_frame_that_only_claims_layers_keeps_the_chain_output() {
        let _gpu = crate::tests::one_gpu_test_at_a_time();
        let Some(mut compositor) = gpu() else { return };
        let mut cache = ResourceCache::new();
        let mut chains = Chains::default();
        let filtered = shot(&graph(&[("cam", true)]));
        let key = GpuKey::Filtered(SourceId::new("cam"));
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
                None,
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
        let cam = SourceId::new("cam");
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
}
