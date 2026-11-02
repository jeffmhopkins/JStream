//! The built-in audio filter kinds (ADR-0026 §6, docs/adr/0026-source-filter-chains.md):
//! one `audio_filter` descriptor per mixer kernel (`gain`, `noise_gate`, `compressor`,
//! `limiter` and `delay`), and the mapping from a filter's settings, complete as the graph
//! stores them, to the kernel the mixer builds ([`spec`]) or updates in place
//! ([`params`]).
//!
//! The mapping drives every audio chain the mixer runs: each audio source's on its own
//! mixer input, the master stage's on every bus that feeds an output, and an output's
//! own on its output tap (ADR-0080). One setting only a rebuild changes: a limiter's
//! `lookahead_ms`, which is its latency, so [`params`] leaves it out and the descriptor
//! marks it not live.
//!
//! A compressor's `key` names another source to duck under (ADR-0028's sidechain key):
//! its detector then hears that source's mixer input, after its fader, mute and
//! activity gain, instead of its own. The key is resolved to a mixer input where the
//! chain runs ([`resolve_keys`]); a key that names no playing input reads as silence, so
//! nothing is ducked. Setting, clearing or re-resolving a key rebuilds the filter.

use jstream_audio::filter::{
    CompressorParams, GateParams, LimiterParams, MAX_DELAY_MS, MAX_LOOKAHEAD_MS,
};
use jstream_audio::{
    AudioError, FilterControl, FilterParams, FilterSpec, InputControl, InputId, MasterControl,
    OutputTapControl,
};
use jstream_platform::HostBlock;
use jstream_protocol::{
    FilterChain, FilterId, KindCategory, KindDescriptor, Media, ParamDescriptor, ParamType,
    Reference, Settings, SourceId,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use crate::plugins::filter::{ClapCatalog, ClapFilter};
use crate::plugins::kinds::CLAP_KIND_PREFIX;
use serde_json::Value;

/// The kind that changes a source's level.
const GAIN: &str = "gain";
/// A compressor's sidechain key: the source it ducks under (ADR-0028).
const KEY: &str = "key";
/// The kind that silences a source below a threshold.
const NOISE_GATE: &str = "noise_gate";
/// The kind that evens out a source's level.
const COMPRESSOR: &str = "compressor";
/// The kind that keeps a source under a ceiling.
const LIMITER: &str = "limiter";
/// The kind that delays a source, to line it up with video.
const DELAY: &str = "delay";

/// The lowest gain and gate range a setting takes, dB: at this value the kernel is
/// silent (`-inf`), since JSON has no infinity.
const SILENT_DB: f64 = -96.0;
/// The highest compressor ratio a setting takes: at this value the curve is a limiter's
/// (`inf`).
const MAX_RATIO: f64 = 100.0;

/// Every built-in audio filter kind.
pub(crate) fn descriptors() -> Vec<KindDescriptor> {
    let gate = GateParams::default();
    let comp = CompressorParams::default();
    let limit = LimiterParams::default();
    vec![
        filter(
            GAIN,
            "Gain",
            vec![float("gain_db", "Gain", SILENT_DB, 24.0, 0.0, "dB")],
        ),
        filter(
            NOISE_GATE,
            "Noise gate",
            vec![
                float(
                    "open_threshold_db",
                    "Open threshold",
                    -96.0,
                    0.0,
                    gate.open_threshold_db,
                    "dB",
                ),
                ParamDescriptor {
                    description: Some(
                        "Where an open gate starts to close. Above the open threshold, it is \
                         the open threshold."
                            .into(),
                    ),
                    ..float(
                        "close_threshold_db",
                        "Close threshold",
                        -96.0,
                        0.0,
                        gate.close_threshold_db,
                        "dB",
                    )
                },
                float("attack_ms", "Attack", 0.0, 1000.0, gate.attack_ms, "ms"),
                float("hold_ms", "Hold", 0.0, 5000.0, gate.hold_ms, "ms"),
                float("release_ms", "Release", 0.0, 5000.0, gate.release_ms, "ms"),
                ParamDescriptor {
                    description: Some("The level while closed. −96 dB is silence.".into()),
                    ..float("range_db", "Range", SILENT_DB, 0.0, SILENT_DB as f32, "dB")
                },
            ],
        ),
        filter(
            COMPRESSOR,
            "Compressor",
            vec![
                float(
                    "threshold_db",
                    "Threshold",
                    -60.0,
                    0.0,
                    comp.threshold_db,
                    "dB",
                ),
                ParamDescriptor {
                    description: Some(format!(
                        "Input dB over the threshold per output dB. {MAX_RATIO} is a limiter's curve."
                    )),
                    ..float("ratio", "Ratio", 1.0, MAX_RATIO, comp.ratio, ":1")
                },
                float("knee_db", "Knee", 0.0, 24.0, comp.knee_db, "dB"),
                float("attack_ms", "Attack", 0.0, 500.0, comp.attack_ms, "ms"),
                float("release_ms", "Release", 0.0, 5000.0, comp.release_ms, "ms"),
                float("makeup_db", "Makeup gain", 0.0, 24.0, comp.makeup_db, "dB"),
                ParamDescriptor {
                    id: KEY.into(),
                    ty: ParamType::Source {
                        media: Some(Media::Audio),
                    },
                    default: Value::Null,
                    label: "Duck under".into(),
                    description: Some(
                        "Another source whose sound drives the compressor instead of this \
                         one's, after its fader and mute: this source ducks while that one \
                         plays, such as music under a microphone. Nothing ducks while that \
                         source is muted or not playing. Changing it rebuilds the filter."
                            .into(),
                    ),
                    group: None,
                    advanced: false,
                    visible_if: None,
                    live: false,
                },
            ],
        ),
        filter(
            LIMITER,
            "Limiter",
            vec![
                float("ceiling_db", "Ceiling", -30.0, 0.0, limit.ceiling_db, "dB"),
                float("release_ms", "Release", 1.0, 5000.0, limit.release_ms, "ms"),
                ParamDescriptor {
                    description: Some(
                        "How far ahead it looks, which is also the delay it adds. Changing \
                         it rebuilds the filter."
                            .into(),
                    ),
                    live: false,
                    ..float(
                        "lookahead_ms",
                        "Look-ahead",
                        0.0,
                        f64::from(MAX_LOOKAHEAD_MS),
                        5.0,
                        "ms",
                    )
                },
            ],
        ),
        filter(
            DELAY,
            "Delay",
            vec![float(
                "delay_ms",
                "Delay",
                0.0,
                f64::from(MAX_DELAY_MS),
                0.0,
                "ms",
            )],
        ),
    ]
}

/// The kernel for a filter of `kind_id` with these settings, or `None` when `kind_id` is
/// not a built-in audio filter kind. A missing or mistyped setting reads as its default,
/// and the mixer still checks every range when it builds the kernel.
pub(crate) fn spec(kind_id: &str, settings: &Settings) -> Option<FilterSpec> {
    let read = Read(settings);
    Some(match kind_id {
        LIMITER => FilterSpec::Limiter {
            lookahead_ms: read.f32("lookahead_ms", 5.0),
            params: read.limiter(),
        },
        DELAY => FilterSpec::Delay {
            // Built to hold the longest delay, so changing it never rebuilds the filter.
            max_ms: MAX_DELAY_MS,
            delay_ms: read.f32("delay_ms", 0.0),
        },
        _ => match params(kind_id, settings)? {
            FilterParams::Gain { gain_db } => FilterSpec::Gain { gain_db },
            FilterParams::NoiseGate(p) => FilterSpec::NoiseGate(p),
            FilterParams::Compressor(p) => FilterSpec::Compressor(p),
            _ => return None,
        },
    })
}

/// The settings to send a filter of `kind_id` already in a chain, or `None` when
/// `kind_id` is not a built-in audio filter kind. A limiter's look-ahead is not among
/// them: it changes only when the filter is rebuilt from [`spec`].
pub(crate) fn params(kind_id: &str, settings: &Settings) -> Option<FilterParams> {
    let read = Read(settings);
    Some(match kind_id {
        GAIN => FilterParams::Gain {
            gain_db: read.db("gain_db", 0.0),
        },
        NOISE_GATE => {
            let d = GateParams::default();
            let open = read.f32("open_threshold_db", d.open_threshold_db);
            FilterParams::NoiseGate(GateParams {
                open_threshold_db: open,
                close_threshold_db: read
                    .f32("close_threshold_db", d.close_threshold_db)
                    .min(open),
                attack_ms: read.f32("attack_ms", d.attack_ms),
                hold_ms: read.f32("hold_ms", d.hold_ms),
                release_ms: read.f32("release_ms", d.release_ms),
                range_db: read.db("range_db", d.range_db),
            })
        }
        COMPRESSOR => {
            let d = CompressorParams::default();
            let ratio = read.f32("ratio", d.ratio);
            FilterParams::Compressor(CompressorParams {
                threshold_db: read.f32("threshold_db", d.threshold_db),
                ratio: if f64::from(ratio) >= MAX_RATIO {
                    f32::INFINITY
                } else {
                    ratio
                },
                knee_db: read.f32("knee_db", d.knee_db),
                attack_ms: read.f32("attack_ms", d.attack_ms),
                release_ms: read.f32("release_ms", d.release_ms),
                makeup_db: read.f32("makeup_db", d.makeup_db),
            })
        }
        LIMITER => FilterParams::Limiter(read.limiter()),
        DELAY => FilterParams::Delay {
            delay_ms: read.f32("delay_ms", 0.0),
        },
        _ => return None,
    })
}

/// One filter of a source's audio chain, as the mixer runs it: enabled, of a built-in
/// kind or a CLAP kind, with its settings as the graph stores them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Chained {
    id: FilterId,
    kind: String,
    settings: Settings,
    /// A keyed compressor's key, resolved to the mixer input it reads where the chain
    /// runs ([`resolve_keys`]): `None` until then, and while the key source isn't
    /// playing.
    key: Option<InputId>,
}

impl Chained {
    /// The source a compressor ducks under, if it is keyed.
    fn key_source(&self) -> Option<SourceId> {
        if self.kind != COMPRESSOR {
            return None;
        }
        match Reference::from_value(self.settings.get(KEY)?)? {
            Reference::Source { source_id } => Some(source_id),
            _ => None,
        }
    }

    /// The kernel the mixer builds for this filter: [`spec`]'s, keyed on [`Self::key`]
    /// when it is a compressor with a key.
    fn spec(&self) -> Option<FilterSpec> {
        match spec(&self.kind, &self.settings)? {
            FilterSpec::Compressor(params) if self.key_source().is_some() => {
                Some(FilterSpec::KeyedCompressor {
                    params,
                    key: self.key,
                })
            }
            spec => Some(spec),
        }
    }

    /// Whether this is a CLAP filter, run by a plugin host (ADR-0035).
    fn is_clap(&self) -> bool {
        self.kind.starts_with(CLAP_KIND_PREFIX)
    }
}

/// The filters the mixer runs for an audio chain of `chains` (a source's, the master
/// stage's or an output's): in order, without the disabled filters and those of a kind
/// the engine runs nothing for (a pack kind), which are bypassed. A CLAP filter is kept even before the scan that describes its
/// kind: the chain reports it and bypasses it until then.
pub(crate) fn chain_of(chains: &jstream_graph::Chains) -> Vec<Chained> {
    chains
        .chain(FilterChain::Audio)
        .iter()
        .filter(|f| {
            f.enabled()
                && (f.kind().starts_with(CLAP_KIND_PREFIX)
                    || spec(f.kind(), f.settings()).is_some())
        })
        .map(|f| Chained {
            id: f.id().clone(),
            kind: f.kind().to_owned(),
            settings: f.settings().clone(),
            key: None,
        })
        .collect()
}

/// `chain` with each keyed compressor's key resolved by `input`, which gives the mixer
/// input a source plays on now, if any.
pub(crate) fn resolve_keys(
    chain: &[Chained],
    input: impl Fn(&SourceId) -> Option<InputId>,
) -> Vec<Chained> {
    chain
        .iter()
        .map(|chained| Chained {
            key: chained.key_source().and_then(|source| input(&source)),
            ..chained.clone()
        })
        .collect()
}

/// Where a [`Chain`] runs: a mixer input, the master chain, or an output tap.
pub(crate) trait FilterHost {
    /// Adds a filter at the end of this host's chain.
    fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError>;

    /// Adds the CLAP filter on `block` at the end of this host's chain (ADR-0035).
    fn add_host_filter(&self, block: HostBlock) -> Result<FilterControl, AudioError>;

    /// The mixer's sample rate and frames per block, which a CLAP filter's host block
    /// is made for.
    fn mixer_format(&self) -> (u32, usize);
}

impl FilterHost for InputControl {
    fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError> {
        InputControl::add_filter(self, spec)
    }

    fn add_host_filter(&self, block: HostBlock) -> Result<FilterControl, AudioError> {
        self.insert_host_filter(usize::MAX, block)
    }

    fn mixer_format(&self) -> (u32, usize) {
        (self.mixer_rate(), self.mixer_block_frames())
    }
}

impl FilterHost for MasterControl {
    fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError> {
        MasterControl::add_filter(self, spec)
    }

    fn add_host_filter(&self, block: HostBlock) -> Result<FilterControl, AudioError> {
        self.insert_host_filter(usize::MAX, block)
    }

    fn mixer_format(&self) -> (u32, usize) {
        (self.mixer_rate(), self.mixer_block_frames())
    }
}

impl FilterHost for OutputTapControl {
    fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError> {
        OutputTapControl::add_filter(self, spec)
    }

    fn add_host_filter(&self, block: HostBlock) -> Result<FilterControl, AudioError> {
        self.insert_host_filter(usize::MAX, block)
    }

    fn mixer_format(&self) -> (u32, usize) {
        (self.mixer_rate(), self.mixer_block_frames())
    }
}

/// Where a [`Chain`]'s CLAP filters get their host blocks.
pub(crate) enum Clap<'a> {
    /// The chain starts and supervises its CLAP filters' hosts itself, from this
    /// catalog once a scan has found plugins: a source's chain, on the program stage's
    /// `jstream-audio-sources` thread.
    Run(Option<&'a ClapCatalog>),
    /// The hosts run elsewhere ([`ClapHosts`] on the program stage's thread), and these
    /// are their ready blocks by filter ID: the master chain's and an output tap's,
    /// kept by the engine's routing under the collection lock, where starting or
    /// stopping a process would hold it up.
    Blocks(&'a BTreeMap<FilterId, HostBlock>),
}

/// The hosts of one chain's CLAP filters, by filter ID (ADR-0035 › 1: one host process
/// per filter instance).
#[derive(Default)]
pub(crate) struct ClapHosts {
    hosts: BTreeMap<FilterId, ClapFilter>,
    /// CLAP filters already reported as not installed, so each is logged once.
    missing: BTreeSet<FilterId>,
}

impl ClapHosts {
    /// Starts a host for each wanted CLAP filter that has none, for a mixer at `rate`
    /// with blocks of `block_frames`, stops those no longer wanted, and ticks the rest.
    /// Returns what to log.
    pub(crate) fn sync(
        &mut self,
        wanted: &[Chained],
        clap: Option<&ClapCatalog>,
        (rate, block_frames): (u32, usize),
        now: Instant,
    ) -> Vec<String> {
        let mut notes = Vec::new();
        let ids: BTreeSet<&FilterId> = wanted
            .iter()
            .filter(|c| c.is_clap())
            .map(|c| &c.id)
            .collect();
        // Dropping a filter stops its host.
        self.hosts.retain(|id, _| ids.contains(id));
        self.missing.retain(|id| ids.contains(id));
        for new in wanted.iter().filter(|c| c.is_clap()) {
            if !self.hosts.contains_key(&new.id) {
                let started = clap.and_then(|catalog| {
                    ClapFilter::new(catalog, &new.kind, rate, block_frames, now)
                });
                match started {
                    Some(filter) => {
                        self.missing.remove(&new.id);
                        self.hosts.insert(new.id.clone(), filter);
                    }
                    None => {
                        if self.missing.insert(new.id.clone()) {
                            notes.push(format!(
                                "{} filter is bypassed: no CLAP plugin found for its kind",
                                new.kind
                            ));
                        }
                        continue;
                    }
                }
            }
            if let Some(filter) = self.hosts.get_mut(&new.id) {
                notes.extend(filter.tick(&new.kind, now));
            }
        }
        notes
    }

    /// Sends each wanted CLAP filter's plugin the settings that changed.
    pub(crate) fn send(&mut self, wanted: &[Chained]) {
        for new in wanted.iter().filter(|new| new.is_clap()) {
            if let Some(filter) = self.hosts.get_mut(&new.id) {
                filter.send(&new.settings);
            }
        }
    }

    /// The block each CLAP filter runs on now, by filter ID.
    pub(crate) fn blocks(&self) -> BTreeMap<FilterId, HostBlock> {
        self.hosts
            .iter()
            .filter_map(|(id, filter)| Some((id.clone(), Arc::clone(filter.block()?))))
            .collect()
    }
}

/// One filter the mixer was last asked to run.
struct Slot {
    chained: Chained,
    /// Its control: `None` for one the mixer refused, and for a CLAP filter whose host
    /// isn't ready, both bypassed.
    control: Option<FilterControl>,
    /// A CLAP filter's block, when it was added on one.
    block: Option<HostBlock>,
}

/// The filters one [`FilterHost`] runs, kept in line with its chain in the collection
/// by [`Self::apply`].
#[derive(Default)]
pub(crate) struct Chain {
    filters: Vec<Slot>,
    /// Its CLAP filters' hosts, kept across rebuilds of the chain, when it runs them
    /// itself ([`Clap::Run`]).
    hosts: ClapHosts,
    /// Filters whose removal the mixer's command queue could not take yet, with a CLAP
    /// filter's block: no new filter goes on that block until they are gone.
    removing: Vec<(FilterControl, Option<HostBlock>)>,
    /// Rebuild on the next pass even if nothing changed: a CLAP filter's block was
    /// still in use.
    retry: bool,
}

impl Chain {
    /// Brings `host`'s chain in line with `wanted`, at `now`. When only settings
    /// changed, and none that rebuilds a filter, each changed built-in filter is updated
    /// in place, so its state (a gate's envelope, a delay line) carries on; otherwise
    /// the chain is rebuilt. Returns what to log: a filter the mixer would not add is
    /// bypassed, and new settings it would not take keep the old ones and are tried
    /// again on the next pass. A refusal is a runtime error on that filter, never an
    /// edit failure (ADR-0026 §6).
    ///
    /// A CLAP filter runs in a host process ([`ClapFilter`]), which this chain starts
    /// and supervises with [`Clap::Run`] or which runs elsewhere with
    /// [`Clap::Blocks`]. It is bypassed until its host is first ready and while it is
    /// disabled, passes delayed dry on its dead block while a crashed host restarts,
    /// and its settings reach the plugin as parameter changes, never through a rebuild.
    /// Without a plugin for its kind, it is bypassed.
    pub(crate) fn apply(
        &mut self,
        host: &impl FilterHost,
        wanted: &[Chained],
        clap: Clap<'_>,
        now: Instant,
    ) -> Vec<String> {
        self.removing.retain(|(control, _)| !control.remove());
        let (mut notes, blocks) = match clap {
            Clap::Run(catalog) => {
                let notes = self.hosts.sync(wanted, catalog, host.mixer_format(), now);
                (notes, self.hosts.blocks())
            }
            Clap::Blocks(blocks) => (Vec::new(), blocks.clone()),
        };
        let same_layout = !self.retry
            && self.filters.len() == wanted.len()
            && self.filters.iter().zip(wanted).all(|(slot, new)| {
                let old = &slot.chained;
                old.id == new.id
                    && old.kind == new.kind
                    && if new.is_clap() {
                        same_block(slot.block.as_ref(), blocks.get(&new.id))
                    } else {
                        !rebuilds(old, new)
                            && (slot.control.is_some() || old.settings == new.settings)
                    }
            });
        if same_layout {
            for (slot, new) in self.filters.iter_mut().zip(wanted) {
                if new.is_clap() || slot.chained == *new {
                    continue;
                }
                if let (Some(control), Some(params)) =
                    (&slot.control, params(&new.kind, &new.settings))
                    && let Err(err) = control.set(params)
                {
                    // The old settings stay recorded, so the next pass tries again.
                    notes.push(format!(
                        "{} filter keeps its previous settings: {err}",
                        new.kind
                    ));
                    continue;
                }
                slot.chained = new.clone();
            }
        } else {
            self.rebuild(host, wanted, &blocks, &mut notes);
        }
        if matches!(clap, Clap::Run(_)) {
            self.hosts.send(wanted);
        }
        notes
    }

    /// Each of its CLAP filters that the mixer runs now, on its host's block, with its
    /// control: on the master chain, the filters whose copies on other buses can run on
    /// hosts of their own (ADR-0086).
    pub(crate) fn clap_controls(&self) -> impl Iterator<Item = (&FilterId, &FilterControl)> {
        self.filters
            .iter()
            .filter_map(|slot| match (&slot.block, &slot.control) {
                (Some(_), Some(control)) => Some((&slot.chained.id, control)),
                _ => None,
            })
    }

    /// Whether every filter it removed has left the mixer's queue: until then a removed
    /// CLAP filter may still run on its block.
    pub(crate) fn settled(&self) -> bool {
        self.removing.is_empty()
    }

    /// How many of its CLAP filters run on a host's block now, for tests.
    #[cfg(test)]
    pub(crate) fn hosted(&self) -> usize {
        self.filters
            .iter()
            .filter(|slot| slot.block.is_some() && slot.control.is_some())
            .count()
    }

    /// Removes every filter from `host` and adds `wanted`'s, in order, each CLAP filter
    /// on its block in `blocks`.
    fn rebuild(
        &mut self,
        host: &impl FilterHost,
        wanted: &[Chained],
        blocks: &BTreeMap<FilterId, HostBlock>,
        notes: &mut Vec<String>,
    ) {
        self.retry = false;
        for slot in self.filters.drain(..) {
            if let Some(control) = slot.control
                && !control.remove()
            {
                self.removing.push((control, slot.block));
            }
        }
        for chained in wanted {
            let (control, block) = if chained.is_clap() {
                let block = blocks.get(&chained.id).cloned();
                let control = match &block {
                    Some(block)
                        if self
                            .removing
                            .iter()
                            .any(|(_, old)| same_block(old.as_ref(), Some(block))) =>
                    {
                        // Its old filter is still on the block: next pass.
                        self.retry = true;
                        None
                    }
                    Some(block) => host
                        .add_host_filter(Arc::clone(block))
                        .map_err(|err| {
                            notes.push(format!("{} filter is bypassed: {err}", chained.kind));
                        })
                        .ok(),
                    None => None,
                };
                (control, block)
            } else {
                let control = chained
                    .spec()
                    .map(|spec| host.add_filter(spec))
                    .and_then(|added| {
                        added
                            .map_err(|err| {
                                notes.push(format!("{} filter is bypassed: {err}", chained.kind));
                            })
                            .ok()
                    });
                (control, None)
            };
            self.filters.push(Slot {
                chained: chained.clone(),
                control,
                block,
            });
        }
    }
}

/// Whether `a` and `b` are the same block, or both none.
pub(crate) fn same_block(a: Option<&HostBlock>, b: Option<&HostBlock>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// Whether going from `old` to `new`, of the same kind, rebuilds the filter: a
/// limiter's look-ahead is its latency, and a compressor's key the input it reads, both
/// fixed when it is built.
fn rebuilds(old: &Chained, new: &Chained) -> bool {
    match old.kind.as_str() {
        LIMITER => old.settings.get("lookahead_ms") != new.settings.get("lookahead_ms"),
        COMPRESSOR => old.settings.get(KEY) != new.settings.get(KEY) || old.key != new.key,
        _ => false,
    }
}

/// A filter's settings, read as kernel values.
struct Read<'a>(&'a Settings);

impl Read<'_> {
    fn f32(&self, id: &str, default: f32) -> f32 {
        self.0
            .get(id)
            .and_then(Value::as_f64)
            .map_or(default, |v| v as f32)
    }

    /// A level in dB, where [`SILENT_DB`] and below is silence.
    fn db(&self, id: &str, default: f32) -> f32 {
        let db = self.f32(id, default);
        if f64::from(db) <= SILENT_DB {
            f32::NEG_INFINITY
        } else {
            db
        }
    }

    fn limiter(&self) -> LimiterParams {
        let d = LimiterParams::default();
        LimiterParams {
            ceiling_db: self.f32("ceiling_db", d.ceiling_db),
            release_ms: self.f32("release_ms", d.release_ms),
        }
    }
}

fn filter(id: &str, name: &str, params: Vec<ParamDescriptor>) -> KindDescriptor {
    KindDescriptor {
        kind_id: id.into(),
        category: KindCategory::AudioFilter,
        name: name.into(),
        version: 1,
        video: false,
        audio: true,
        max_outset_px: None,
        params,
    }
}

/// A live number from `min` to `max`, in `unit`.
fn float(id: &str, label: &str, min: f64, max: f64, default: f32, unit: &str) -> ParamDescriptor {
    ParamDescriptor {
        id: id.into(),
        ty: ParamType::Float {
            min: Some(min),
            max: Some(max),
            step: None,
            unit: Some(unit.into()),
        },
        default: Value::from(f64::from(default)),
        label: label.into(),
        description: None,
        group: None,
        advanced: false,
        visible_if: None,
        live: true,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Chain, Chained, Clap, ClapHosts, SILENT_DB, descriptors, params, resolve_keys, spec,
    };
    use jstream_audio::filter::{CompressorParams, GateParams, LimiterParams};
    use jstream_audio::{FilterParams, FilterSpec, Mixer, MixerConfig};
    use jstream_media::{AudioFormat, ChannelLayout, SampleFormat};
    use jstream_protocol::{FilterId, Settings, SourceId};
    use serde_json::{Value, json};
    use std::time::Instant;

    fn defaults(kind: &str) -> Settings {
        let descriptor = descriptors()
            .into_iter()
            .find(|k| k.kind_id == kind)
            .expect("described");
        descriptor
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect()
    }

    /// Each kind's defaults are the kernel's defaults, so a filter added without
    /// settings sounds as the mixer's own default would.
    #[test]
    fn defaults_build_the_kernels_defaults() {
        assert_eq!(
            spec("gain", &defaults("gain")),
            Some(FilterSpec::Gain { gain_db: 0.0 })
        );
        assert_eq!(
            spec("noise_gate", &defaults("noise_gate")),
            Some(FilterSpec::NoiseGate(GateParams::default()))
        );
        assert_eq!(
            spec("compressor", &defaults("compressor")),
            Some(FilterSpec::Compressor(CompressorParams::default()))
        );
        assert_eq!(
            spec("limiter", &defaults("limiter")),
            Some(FilterSpec::Limiter {
                lookahead_ms: 5.0,
                params: LimiterParams::default()
            })
        );
        assert_eq!(
            spec("delay", &defaults("delay")),
            Some(FilterSpec::Delay {
                max_ms: 5000.0,
                delay_ms: 0.0
            })
        );
        assert_eq!(spec("color", &Settings::new()), None);
        assert_eq!(params("tint", &Settings::new()), None);
    }

    /// The edges of a range stand for what JSON can't hold, and a close threshold above
    /// the open one is the open one: every in-range setting builds a valid kernel.
    #[test]
    fn range_edges_map_to_the_kernels_extremes() {
        let mut gate = defaults("noise_gate");
        gate.insert("close_threshold_db", json!(-10.0));
        gate.insert("range_db", json!(SILENT_DB));
        let Some(FilterParams::NoiseGate(p)) = params("noise_gate", &gate) else {
            panic!("a gate");
        };
        assert_eq!(p.close_threshold_db, p.open_threshold_db);
        assert_eq!(p.range_db, f32::NEG_INFINITY);
        assert_eq!(p.validate(), Ok(()));

        let mut comp = defaults("compressor");
        comp.insert("ratio", json!(100.0));
        let Some(FilterParams::Compressor(p)) = params("compressor", &comp) else {
            panic!("a compressor");
        };
        assert_eq!(p.ratio, f32::INFINITY);

        let mut gain = defaults("gain");
        gain.insert("gain_db", json!(-96));
        assert_eq!(
            params("gain", &gain),
            Some(FilterParams::Gain {
                gain_db: f32::NEG_INFINITY
            })
        );
        gain.insert("gain_db", Value::Null);
        assert_eq!(
            params("gain", &gain),
            Some(FilterParams::Gain { gain_db: 0.0 })
        );
    }

    fn chained(kind: &str, entries: &[(&str, Value)]) -> Chained {
        let mut settings = defaults(kind);
        for (id, value) in entries {
            settings.insert(*id, value.clone());
        }
        Chained {
            id: FilterId::new(kind),
            kind: kind.into(),
            settings,
            key: None,
        }
    }

    /// A compressor with a key is built keyed on the key source's input, resolved where
    /// the chain runs; setting, clearing or re-resolving the key rebuilds it, and its
    /// other settings still change in place.
    #[test]
    fn a_keyed_compressor_rebuilds_when_its_key_changes() {
        let mut mixer = Mixer::new(MixerConfig::default()).expect("mixer");
        let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("format");
        let (_producer, input) = mixer.add_input(format).expect("input");
        let (_music, key) = mixer.add_input(format).expect("key input");
        let mic = SourceId::new("mic");
        let keyed = |threshold: f64| {
            [chained(
                "compressor",
                &[
                    ("threshold_db", json!(threshold)),
                    ("key", json!({"type": "source", "source_id": "mic"})),
                ],
            )]
        };
        assert_eq!(keyed(-20.0)[0].key_source(), Some(mic.clone()));
        assert!(matches!(
            keyed(-20.0)[0].spec(),
            Some(FilterSpec::KeyedCompressor { key: None, .. })
        ));
        let id = |chain: &Chain| chain.filters[0].control.as_ref().map(|c| c.id());
        let mut chain = Chain::default();
        let apply = |chain: &mut Chain, wanted: &[Chained]| {
            assert!(
                chain
                    .apply(&input, wanted, Clap::Run(None), Instant::now())
                    .is_empty()
            );
        };

        apply(&mut chain, &[chained("compressor", &[])]);
        let plain = id(&chain);
        assert!(plain.is_some());
        // Keyed on a source that isn't playing: rebuilt, reading silence.
        apply(&mut chain, &resolve_keys(&keyed(-20.0), |_| None));
        let unresolved = id(&chain);
        assert_ne!(unresolved, plain, "rebuilt keyed");
        // The key source starts playing: rebuilt on its input.
        let playing = |source: &SourceId| (*source == mic).then(|| key.id());
        let resolved = resolve_keys(&keyed(-20.0), playing);
        assert_eq!(resolved[0].key, Some(key.id()));
        apply(&mut chain, &resolved);
        let on_key = id(&chain);
        assert_ne!(on_key, unresolved, "rebuilt on the key's input");
        // Another setting changes in place.
        apply(&mut chain, &resolve_keys(&keyed(-30.0), playing));
        assert_eq!(id(&chain), on_key, "updated in place");
        // Clearing the key rebuilds it unkeyed.
        apply(
            &mut chain,
            &[chained("compressor", &[("threshold_db", json!(-30.0))])],
        );
        assert_ne!(id(&chain), on_key, "rebuilt unkeyed");
        // A key on any other kind is no key.
        let gain = chained(
            "gain",
            &[("key", json!({"type": "source", "source_id": "mic"}))],
        );
        assert_eq!(gain.key_source(), None);
    }

    /// A settings change updates the filter in place; a new kind, a reorder, or a
    /// limiter's look-ahead rebuilds the chain; an empty chain removes every filter.
    #[test]
    fn a_chain_updates_in_place_or_rebuilds() {
        let mut mixer = Mixer::new(MixerConfig::default()).expect("mixer");
        let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("format");
        let (_producer, input) = mixer.add_input(format).expect("input");
        let mut chain = Chain::default();
        let ids = |chain: &Chain| -> Vec<_> {
            chain
                .filters
                .iter()
                .map(|s| s.control.as_ref().map(|c| (c.id(), c.latency_frames())))
                .collect()
        };

        let first = [chained("gain", &[]), chained("limiter", &[])];
        assert!(
            chain
                .apply(&input, &first, Clap::Run(None), Instant::now())
                .is_empty()
        );
        let built = ids(&chain);
        // 5 ms of look-ahead at 48 kHz.
        assert!(
            matches!(built[..], [Some((_, 0)), Some((_, 240))]),
            "{built:?}"
        );

        let louder = [
            chained("gain", &[("gain_db", json!(6))]),
            chained("limiter", &[]),
        ];
        assert!(
            chain
                .apply(&input, &louder, Clap::Run(None), Instant::now())
                .is_empty()
        );
        assert_eq!(ids(&chain), built, "updated in place");

        let longer = [
            chained("gain", &[("gain_db", json!(6))]),
            chained("limiter", &[("lookahead_ms", json!(10))]),
        ];
        assert!(
            chain
                .apply(&input, &longer, Clap::Run(None), Instant::now())
                .is_empty()
        );
        let rebuilt = ids(&chain);
        assert_ne!(rebuilt[0], built[0]);
        assert!(matches!(rebuilt[1], Some((_, 480))), "{rebuilt:?}");

        let reordered = [longer[1].clone(), longer[0].clone()];
        assert!(
            chain
                .apply(&input, &reordered, Clap::Run(None), Instant::now())
                .is_empty()
        );
        assert_ne!(ids(&chain)[1], rebuilt[0]);

        assert!(
            chain
                .apply(&input, &[], Clap::Run(None), Instant::now())
                .is_empty()
        );
        assert!(chain.filters.is_empty());
    }

    #[test]
    fn refused_settings_are_kept_off_the_record_and_retried() {
        let config = MixerConfig::default();
        let block = config.block_frames;
        let mut mixer = Mixer::new(config).expect("mixer");
        let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("format");
        let (_producer, input) = mixer.add_input(format).expect("input");
        let mut chain = Chain::default();
        // Tenths of a dB below unity: in range, and a new value at every step.
        let gain = |step: u32| {
            [chained(
                "gain",
                &[("gain_db", json!(-f64::from(step) / 10.0))],
            )]
        };
        assert!(
            chain
                .apply(&input, &gain(0), Clap::Run(None), Instant::now())
                .is_empty()
        );

        // Nothing mixes, so nothing drains the mixer's bounded command queue: in-place
        // changes fill it until one is refused.
        let mut accepted = 0;
        let (step, reasons) = (1..=200)
            .find_map(|step| {
                let refused = chain.apply(&input, &gain(step), Clap::Run(None), Instant::now());
                if refused.is_empty() {
                    accepted = step;
                    None
                } else {
                    Some((step, refused))
                }
            })
            .expect("the command queue fills");
        assert!(
            matches!(&reasons[..], [reason] if reason.contains("keeps its previous settings")
                && reason.contains("queue is full")),
            "{reasons:?}"
        );
        assert_eq!(
            chain.filters[0].chained,
            gain(accepted)[0],
            "the refused change is not recorded"
        );

        // Mixing drains the queue, and the next pass applies the same wanted chain.
        let mut out = vec![0.0; block * 2];
        mixer.mix(0, &mut out).expect("mix");
        let again = chain.apply(&input, &gain(step), Clap::Run(None), Instant::now());
        assert!(again.is_empty(), "{again:?}");
        assert_eq!(chain.filters[0].chained, gain(step)[0]);
        assert!(
            chain
                .apply(&input, &gain(step), Clap::Run(None), Instant::now())
                .is_empty(),
            "nothing left to retry"
        );
    }

    /// The master chain and an output tap run a CLAP filter on the block of a host
    /// run elsewhere ([`ClapHosts`], as the program stage's thread does): bypassed
    /// until the host is ready, then on its block, which the host's settings reach.
    #[test]
    fn the_master_chain_runs_a_clap_filter_on_hosts_run_elsewhere() {
        use crate::plugins::filter::ClapCatalog;
        use crate::plugins::supervisor::SupervisorPolicy;
        use jstream_audio::BusId;
        use jstream_platform::plugin_host::fake::{FAKE_LATENCY, FakePluginHosts};
        use jstream_plugin_proto::{DEFAULT_DEPTH, ParamEvent};
        use std::collections::BTreeMap;
        use std::sync::Arc;
        use std::time::Duration;

        let config = MixerConfig::default();
        let block = config.block_frames;
        let mut mixer = Mixer::new(config).expect("mixer");
        let fake = FakePluginHosts::new();
        let catalog = ClapCatalog {
            hosts: Arc::new(fake.clone()),
            plugins: BTreeMap::from([(
                "clap/org.example.gain".to_owned(),
                ("a.clap".into(), "org.example.gain".to_owned()),
            )]),
            policy: SupervisorPolicy::default(),
        };
        let wanted = [
            chained("gain", &[]),
            Chained {
                id: FilterId::new("verb"),
                kind: "clap/org.example.gain".into(),
                settings: [("1", json!(0.5))].into_iter().collect(),
                key: None,
            },
        ];
        let master = mixer.master();
        let tap = mixer.add_output_tap(BusId::MASTER).expect("a tap");
        let format = (master.mixer_rate(), master.mixer_block_frames());
        let mut hosts = ClapHosts::default();
        let mut chain = Chain::default();
        let mut out = vec![0.0; block * 2];
        let mut pass = |hosts: &mut ClapHosts, chain: &mut Chain, mixer: &mut Mixer| {
            let mut notes = hosts.sync(&wanted, Some(&catalog), format, Instant::now());
            hosts.send(&wanted);
            let blocks = hosts.blocks();
            notes.extend(chain.apply(&master, &wanted, Clap::Blocks(&blocks), Instant::now()));
            mixer.mix(0, &mut out).expect("mix");
            notes
        };
        let notes = pass(&mut hosts, &mut chain, &mut mixer);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(chain.filters[0].control.is_some());
        let deadline = Instant::now() + Duration::from_secs(5);
        while chain.filters[1].control.is_none() {
            assert!(Instant::now() < deadline, "the host never got ready");
            std::thread::sleep(Duration::from_millis(1));
            let notes = pass(&mut hosts, &mut chain, &mut mixer);
            assert!(notes.is_empty(), "{notes:?}");
        }
        let host_latency = DEFAULT_DEPTH as usize * block + FAKE_LATENCY as usize;
        assert_eq!(
            chain.filters[1]
                .control
                .as_ref()
                .map(|c| c.latency_frames()),
            Some(host_latency)
        );
        let on = chain.filters[1].block.clone().expect("a block");
        assert_eq!(
            (*on).as_ref().params_in.pop(),
            Some(ParamEvent { id: 1, value: 0.5 }),
            "its settings reach the plugin"
        );

        // An output tap's chain, with hosts of its own.
        let mut tap_hosts = ClapHosts::default();
        let mut tap_chain = Chain::default();
        while tap_chain
            .filters
            .get(1)
            .is_none_or(|slot| slot.control.is_none())
        {
            assert!(Instant::now() < deadline, "the tap's host never got ready");
            let notes = tap_hosts.sync(&wanted, Some(&catalog), format, Instant::now());
            assert!(notes.is_empty(), "{notes:?}");
            let blocks = tap_hosts.blocks();
            let notes = tap_chain.apply(&tap, &wanted, Clap::Blocks(&blocks), Instant::now());
            assert!(notes.is_empty(), "{notes:?}");
            mixer.mix(0, &mut out).expect("mix");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(fake.hosts().len(), 2, "one host per filter instance");

        // Dropped: its host stops, and the chain takes it off.
        hosts.sync(&[], Some(&catalog), format, Instant::now());
        assert!(fake.hosts()[0].has_exited(), "its host stopped");
        let blocks = hosts.blocks();
        assert!(
            chain
                .apply(&master, &wanted[..1], Clap::Blocks(&blocks), Instant::now())
                .is_empty()
        );
        assert_eq!(chain.filters.len(), 1);
    }

    /// A CLAP filter is bypassed until its host is ready, then runs on the host's
    /// block; a rebuilt chain keeps the same host, and dropping the filter stops it. A
    /// CLAP filter without a plugin is reported once and bypassed.
    #[test]
    fn a_clap_filter_runs_on_its_hosts_block() {
        use crate::plugins::filter::ClapCatalog;
        use crate::plugins::supervisor::SupervisorPolicy;
        use jstream_platform::plugin_host::fake::{FAKE_LATENCY, FakePluginHosts};
        use jstream_plugin_proto::DEFAULT_DEPTH;
        use std::collections::BTreeMap;
        use std::sync::Arc;
        use std::time::Duration;

        let config = MixerConfig::default();
        let block = config.block_frames;
        let mut mixer = Mixer::new(config).expect("mixer");
        let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("format");
        let (_producer, input) = mixer.add_input(format).expect("input");
        let fake = FakePluginHosts::new();
        let catalog = ClapCatalog {
            hosts: Arc::new(fake.clone()),
            plugins: BTreeMap::from([(
                "clap/org.example.gain".to_owned(),
                ("a.clap".into(), "org.example.gain".to_owned()),
            )]),
            policy: SupervisorPolicy {
                first_retry: Duration::from_millis(20),
                ..SupervisorPolicy::default()
            },
        };
        let clap = |id: &str, kind: &str| Chained {
            id: FilterId::new(id),
            kind: kind.into(),
            settings: [("1", json!(0.5))].into_iter().collect(),
            key: None,
        };
        let wanted = [chained("gain", &[]), clap("verb", "clap/org.example.gain")];
        let mut chain = Chain::default();
        let mut out = vec![0.0; block * 2];
        let mut pass = |chain: &mut Chain, mixer: &mut Mixer, wanted: &[Chained]| {
            let notes = chain.apply(&input, wanted, Clap::Run(Some(&catalog)), Instant::now());
            mixer.mix(0, &mut out).expect("mix");
            notes
        };
        // The host loads until released, so a fast host thread can't be ready within
        // the first pass.
        fake.start_loading(true);
        assert!(pass(&mut chain, &mut mixer, &wanted).is_empty());
        assert!(
            chain.filters[1].control.is_none(),
            "bypassed while starting"
        );
        fake.start_loading(false);
        fake.hosts()[0].finish_loading();
        let deadline = Instant::now() + Duration::from_secs(5);
        while chain.filters[1].control.is_none() {
            assert!(Instant::now() < deadline, "the host never got ready");
            std::thread::sleep(Duration::from_millis(1));
            let notes = pass(&mut chain, &mut mixer, &wanted);
            assert!(notes.is_empty(), "{notes:?}");
        }
        let latency = |chain: &Chain| {
            chain.filters[1]
                .control
                .as_ref()
                .map(jstream_audio::FilterControl::latency_frames)
        };
        let host_latency = DEFAULT_DEPTH as usize * block + FAKE_LATENCY as usize;
        assert_eq!(latency(&chain), Some(host_latency));

        // Moved behind a new filter: rebuilt, on the same host.
        let rebuilt = [
            chained("delay", &[]),
            chained("gain", &[]),
            clap("verb", "clap/org.example.gain"),
        ];
        assert!(pass(&mut chain, &mut mixer, &rebuilt).is_empty());
        assert_eq!(
            chain.filters[2]
                .control
                .as_ref()
                .map(|c| c.latency_frames()),
            Some(host_latency)
        );
        assert_eq!(fake.hosts().len(), 1);

        // The host crashes: the filter stays on the dead block, passing delayed dry with
        // the same latency, until the restarted host is ready on a new block.
        let first = chain.filters[2].block.clone().expect("a block");
        fake.hosts()[0].crash();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "the crash was never seen");
            std::thread::sleep(Duration::from_millis(1));
            let notes = pass(&mut chain, &mut mixer, &rebuilt);
            if notes.iter().any(|note| note.contains("crashed")) {
                break;
            }
        }
        let on_dead_block = &chain.filters[2];
        assert!(super::same_block(
            on_dead_block.block.as_ref(),
            Some(&first)
        ));
        assert_eq!(
            on_dead_block.control.as_ref().map(|c| c.latency_frames()),
            Some(host_latency)
        );
        while super::same_block(chain.filters[2].block.as_ref(), Some(&first)) {
            assert!(Instant::now() < deadline, "the host never restarted");
            std::thread::sleep(Duration::from_millis(1));
            pass(&mut chain, &mut mixer, &rebuilt);
        }
        assert_eq!(fake.hosts().len(), 2);
        assert!(chain.filters[2].block.is_some());
        assert_eq!(
            chain.filters[2]
                .control
                .as_ref()
                .map(|c| c.latency_frames()),
            Some(host_latency)
        );

        // A second instance of a plugin no scan found: reported once, bypassed.
        let missing = [
            clap("verb", "clap/org.example.gain"),
            clap("other", "clap/org.example.missing"),
        ];
        let notes = pass(&mut chain, &mut mixer, &missing);
        assert!(
            matches!(&notes[..], [note] if note.contains("no CLAP plugin")),
            "{notes:?}"
        );
        assert!(chain.filters[1].control.is_none());
        assert!(
            pass(&mut chain, &mut mixer, &missing).is_empty(),
            "reported once"
        );

        // Dropped while its host runs: the host is told to stop.
        assert!(!fake.hosts()[1].has_exited());
        assert!(pass(&mut chain, &mut mixer, &[]).is_empty());
        assert!(fake.hosts()[1].has_exited(), "its host stopped");
        assert!(
            fake.hosts()[1]
                .received()
                .contains(&jstream_plugin_proto::control::ToHost::Stop),
            "{:?}",
            fake.hosts()[1].received()
        );
    }

    /// A CLAP filter whose host crashes `max_crashes` times in a row is disabled: taken
    /// off its block and bypassed, and not restarted.
    #[test]
    fn a_disabled_clap_filter_is_bypassed() {
        use crate::plugins::filter::ClapCatalog;
        use crate::plugins::supervisor::SupervisorPolicy;
        use jstream_platform::plugin_host::fake::FakePluginHosts;
        use std::collections::BTreeMap;
        use std::sync::Arc;
        use std::time::Duration;

        let config = MixerConfig::default();
        let block = config.block_frames;
        let mut mixer = Mixer::new(config).expect("mixer");
        let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("format");
        let (_producer, input) = mixer.add_input(format).expect("input");
        let fake = FakePluginHosts::new();
        let first_retry = Duration::from_millis(20);
        let catalog = ClapCatalog {
            hosts: Arc::new(fake.clone()),
            plugins: BTreeMap::from([(
                "clap/org.example.gain".to_owned(),
                ("a.clap".into(), "org.example.gain".to_owned()),
            )]),
            policy: SupervisorPolicy {
                first_retry,
                max_crashes: 1,
                ..SupervisorPolicy::default()
            },
        };
        let wanted = [Chained {
            id: FilterId::new("verb"),
            kind: "clap/org.example.gain".into(),
            settings: Settings::new(),
            key: None,
        }];
        let mut chain = Chain::default();
        let mut out = vec![0.0; block * 2];
        let mut pass = |chain: &mut Chain, mixer: &mut Mixer| {
            let notes = chain.apply(&input, &wanted, Clap::Run(Some(&catalog)), Instant::now());
            mixer.mix(0, &mut out).expect("mix");
            notes
        };
        pass(&mut chain, &mut mixer);
        let deadline = Instant::now() + Duration::from_secs(5);
        while chain.filters[0].control.is_none() {
            assert!(Instant::now() < deadline, "the host never got ready");
            std::thread::sleep(Duration::from_millis(1));
            pass(&mut chain, &mut mixer);
        }
        fake.hosts()[0].crash();
        loop {
            assert!(Instant::now() < deadline, "the crash was never seen");
            std::thread::sleep(Duration::from_millis(1));
            if pass(&mut chain, &mut mixer)
                .iter()
                .any(|note| note.contains("disabled"))
            {
                break;
            }
        }
        // Past when a restart would have come, it is still off and alone.
        let until = Instant::now() + first_retry * 4;
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
            pass(&mut chain, &mut mixer);
        }
        assert!(chain.filters[0].block.is_none());
        assert!(chain.filters[0].control.is_none());
        assert_eq!(fake.hosts().len(), 1, "never restarted");
    }
}
