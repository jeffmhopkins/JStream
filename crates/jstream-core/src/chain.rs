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
//! Each filter draws into a layer of the compositor's [`ResourceCache`], under
//! [`GpuKey::Chain`], with the last one under [`GpuKey::Filtered`], so a steady chain
//! allocates no output texture. A filter that no shot keeps is retired into the cache
//! ([`ResourceCache::retire_filter`]), which hands it back with the images it reclaims
//! once the GPU is done with it, so the render thread drops it on its collector
//! (R-RT-003).
//!
//! A chain whose filter fails to draw (a layer past the adapter's limit, a shader the
//! driver refuses) draws nothing for that frame; the failure is logged once per filter.
//!
//! Docs: crates/jstream-core/README.md#design, docs/adr/0026-source-filter-chains.md

use std::collections::{BTreeMap, BTreeSet};

use jstream_graph::Source;
use jstream_media::MediaTime;
use jstream_protocol::{FilterChain, FilterId, Settings, SourceId};
use jstream_render::{Compositor, FilterClock, RenderError, ResourceCache, VideoFilter};

use crate::snapshot::GpuKey;

/// One filter of a [`Chain`], as the collection holds it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChainFilter {
    pub(crate) id: FilterId,
    pub(crate) kind: String,
    pub(crate) settings: Settings,
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
    /// effect pack that isn't loaded). Such a filter is bypassed.
    pub(crate) fn of(source: &Source, input: GpuKey) -> Option<Self> {
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
    /// Whether a failure to draw has been logged, so a filter that keeps failing logs
    /// once.
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
                sync(running, &chain.filters, &mut self.retired);
            }
        }
        for filter in self.retired.drain(..) {
            cache.retire_filter(filter);
        }
        // Every output layer first, so the draws below read and write the cache
        // without borrowing it mutably.
        for (source, chain) in chains {
            let Some(running) = self.running.get(source) else {
                continue;
            };
            let Some(input) = cache.get(&chain.input) else {
                self.failed.insert(source.clone());
                continue;
            };
            let mut size = (input.width(), input.height());
            let last = running.len().saturating_sub(1);
            for (at, filter) in running.iter().enumerate() {
                size = filter.filter.output_size(size.0, size.1);
                let key = output_key(source, at, last);
                if let Err(error) = cache.layer(compositor, &key, size.0, size.1) {
                    tracing::warn!(
                        source = %source,
                        filter = %filter.id,
                        %error,
                        "video filter output can't be made; the source draws nothing"
                    );
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
            let last = running.len().saturating_sub(1);
            let mut input = chain.input.clone();
            for (at, filter) in running.iter_mut().enumerate() {
                let key = output_key(source, at, last);
                let drawn = match (cache.get(&input), cache.get(&key)) {
                    (Some(from), Some(to)) => {
                        compositor.apply_filter_into(&mut filter.filter, from, to, clock)
                    }
                    _ => Err(RenderError::Invalid {
                        what: "cache",
                        detail: format!("no image for {input:?} or {key:?}"),
                    }),
                };
                if let Err(error) = drawn {
                    if !filter.warned {
                        filter.warned = true;
                        tracing::warn!(
                            source = %source,
                            filter = %filter.id,
                            kind = %filter.kind,
                            %error,
                            "video filter failed to draw; the source draws nothing"
                        );
                    }
                    self.failed.insert(source.clone());
                    break;
                }
                input = key;
            }
        }
    }

    /// Whether `key`, an image a layer shows, is a chain output that failed this frame.
    pub(crate) fn failed(&self, key: &GpuKey) -> bool {
        matches!(key, GpuKey::Filtered(source) if self.failed.contains(source))
    }

    /// How many filters run, for tests.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
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
/// list, keeping each filter whose ID and kind it finds. Allocates nothing when nothing
/// changed.
fn sync(running: &mut Vec<Running>, filters: &[ChainFilter], retired: &mut Vec<VideoFilter>) {
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
                    warned: false,
                },
                // The plan only keeps kinds this engine draws.
                None => continue,
            },
        };
        running.push(entry);
    }
    retired.extend(old.into_iter().map(|r| r.filter));
}
