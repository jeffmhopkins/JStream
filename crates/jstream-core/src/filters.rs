//! The built-in audio filter kinds (ADR-0026 §6, docs/adr/0026-source-filter-chains.md):
//! one `audio_filter` descriptor per mixer kernel (`gain`, `noise_gate`, `compressor`,
//! `limiter` and `delay`), and the mapping from a filter's settings, complete as the graph
//! stores them, to the kernel the mixer builds ([`spec`]) or updates in place
//! ([`params`]).
//!
//! The mapping drives every audio chain the mixer runs: each audio source's on its own
//! mixer input, the master stage's on every bus that feeds an output, and an output's
//! own on its output tap (ADR-0027). One setting only a rebuild changes: a limiter's
//! `lookahead_ms`, which is its latency, so [`params`] leaves it out and the descriptor
//! marks it not live.

use jstream_audio::filter::{
    CompressorParams, GateParams, LimiterParams, MAX_DELAY_MS, MAX_LOOKAHEAD_MS,
};
use jstream_audio::{
    AudioError, FilterControl, FilterParams, FilterSpec, InputControl, MasterControl,
    OutputTapControl,
};
use jstream_protocol::{
    FilterChain, KindCategory, KindDescriptor, ParamDescriptor, ParamType, Settings,
};
use serde_json::Value;

/// The kind that changes a source's level.
const GAIN: &str = "gain";
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
/// kind, with its settings complete as the graph stores them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Chained {
    kind: String,
    settings: Settings,
}

/// The filters the mixer runs for an audio chain of `chains` (a source's, the master
/// stage's or an output's): in order, without the disabled filters and those of a kind
/// the engine has no kernel for (a pack kind), which are bypassed.
pub(crate) fn chain_of(chains: &jstream_graph::Chains) -> Vec<Chained> {
    chains
        .chain(FilterChain::Audio)
        .iter()
        .filter(|f| f.enabled() && spec(f.kind(), f.settings()).is_some())
        .map(|f| Chained {
            kind: f.kind().to_owned(),
            settings: f.settings().clone(),
        })
        .collect()
}

/// Where a [`Chain`] runs: a mixer input, the master chain, or an output tap.
pub(crate) trait FilterHost {
    /// Adds a filter at the end of this host's chain.
    fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError>;
}

impl FilterHost for InputControl {
    fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError> {
        InputControl::add_filter(self, spec)
    }
}

impl FilterHost for MasterControl {
    fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError> {
        MasterControl::add_filter(self, spec)
    }
}

impl FilterHost for OutputTapControl {
    fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError> {
        OutputTapControl::add_filter(self, spec)
    }
}

/// The filters one [`FilterHost`] runs, kept in line with its chain in the collection
/// by [`Self::apply`].
#[derive(Default)]
pub(crate) struct Chain {
    /// What the input was last asked to run, and each filter's control: `None` for one
    /// the mixer refused, which is bypassed.
    filters: Vec<(Chained, Option<FilterControl>)>,
    /// Filters whose removal the mixer's command queue could not take yet.
    removing: Vec<FilterControl>,
}

impl Chain {
    /// Brings `host`'s chain in line with `wanted`. When only settings changed, and
    /// none that rebuilds a filter, each changed filter is updated in place, so its
    /// state (a gate's envelope, a delay line) carries on; otherwise the chain is
    /// rebuilt. Returns what the mixer refused, to log: a filter it would not add is
    /// bypassed, and one whose new settings it would not take keeps its old ones and
    /// is tried again on the next pass. A refusal is a runtime error on that filter,
    /// never an edit failure (ADR-0026 §6).
    pub(crate) fn apply(&mut self, host: &impl FilterHost, wanted: &[Chained]) -> Vec<String> {
        self.removing.retain(|control| !control.remove());
        let current: Vec<&Chained> = self.filters.iter().map(|(chained, _)| chained).collect();
        if current.iter().copied().eq(wanted) {
            return Vec::new();
        }
        let mut refused = Vec::new();
        let in_place = self.filters.len() == wanted.len()
            && self
                .filters
                .iter()
                .zip(wanted)
                .all(|((old, control), new)| {
                    control.is_some() && old.kind == new.kind && !rebuilds(old, new)
                });
        if in_place {
            for ((old, control), new) in self.filters.iter_mut().zip(wanted) {
                if old == new {
                    continue;
                }
                if let (Some(control), Some(params)) = (control, params(&new.kind, &new.settings))
                    && let Err(err) = control.set(params)
                {
                    // `old` stays as it was, so the next pass tries again.
                    refused.push(format!(
                        "{} filter keeps its previous settings: {err}",
                        new.kind
                    ));
                    continue;
                }
                *old = new.clone();
            }
            return refused;
        }
        for (_, control) in self.filters.drain(..) {
            if let Some(control) = control
                && !control.remove()
            {
                self.removing.push(control);
            }
        }
        for chained in wanted {
            let control = spec(&chained.kind, &chained.settings)
                .map(|spec| host.add_filter(spec))
                .and_then(|added| {
                    added
                        .map_err(|err| {
                            refused.push(format!("{} filter is bypassed: {err}", chained.kind));
                        })
                        .ok()
                });
            self.filters.push((chained.clone(), control));
        }
        refused
    }
}

/// Whether going from `old` to `new`, of the same kind, rebuilds the filter: a
/// limiter's look-ahead is its latency, fixed when it is built.
fn rebuilds(old: &Chained, new: &Chained) -> bool {
    old.kind == LIMITER && old.settings.get("lookahead_ms") != new.settings.get("lookahead_ms")
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
    use super::{Chain, Chained, SILENT_DB, descriptors, params, spec};
    use jstream_audio::filter::{CompressorParams, GateParams, LimiterParams};
    use jstream_audio::{FilterParams, FilterSpec, Mixer, MixerConfig};
    use jstream_media::{AudioFormat, ChannelLayout, SampleFormat};
    use jstream_protocol::Settings;
    use serde_json::{Value, json};

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
            kind: kind.into(),
            settings,
        }
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
                .map(|(_, c)| c.as_ref().map(|c| (c.id(), c.latency_frames())))
                .collect()
        };

        let first = [chained("gain", &[]), chained("limiter", &[])];
        assert!(chain.apply(&input, &first).is_empty());
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
        assert!(chain.apply(&input, &louder).is_empty());
        assert_eq!(ids(&chain), built, "updated in place");

        let longer = [
            chained("gain", &[("gain_db", json!(6))]),
            chained("limiter", &[("lookahead_ms", json!(10))]),
        ];
        assert!(chain.apply(&input, &longer).is_empty());
        let rebuilt = ids(&chain);
        assert_ne!(rebuilt[0], built[0]);
        assert!(matches!(rebuilt[1], Some((_, 480))), "{rebuilt:?}");

        let reordered = [longer[1].clone(), longer[0].clone()];
        assert!(chain.apply(&input, &reordered).is_empty());
        assert_ne!(ids(&chain)[1], rebuilt[0]);

        assert!(chain.apply(&input, &[]).is_empty());
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
        assert!(chain.apply(&input, &gain(0)).is_empty());

        // Nothing mixes, so nothing drains the mixer's bounded command queue: in-place
        // changes fill it until one is refused.
        let mut accepted = 0;
        let (step, reasons) = (1..=200)
            .find_map(|step| {
                let refused = chain.apply(&input, &gain(step));
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
            chain.filters[0].0,
            gain(accepted)[0],
            "the refused change is not recorded"
        );

        // Mixing drains the queue, and the next pass applies the same wanted chain.
        let mut out = vec![0.0; block * 2];
        mixer.mix(0, &mut out).expect("mix");
        let again = chain.apply(&input, &gain(step));
        assert!(again.is_empty(), "{again:?}");
        assert_eq!(chain.filters[0].0, gain(step)[0]);
        assert!(
            chain.apply(&input, &gain(step)).is_empty(),
            "nothing left to retry"
        );
    }
}
