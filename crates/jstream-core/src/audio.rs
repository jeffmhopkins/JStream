//! The program mixer follows the collection's audio (ADR-0028,
//! docs/adr/0028-audio-model-v2.md): each bus of the collection gets a mixer bus, with
//! its fader and mute, and each source with a mixer input gets its fader, its mute and
//! its sends. The engine syncs after every applied batch, undo and redo, and when the
//! program stage opens its mixer; the mixer's own controls are a few atomics each, so a
//! sync never waits on the mixer thread (docs/RULES.md#R-RT-003).
//!
//! While the mixer runs, its levels go out as `meters.tick` about ten times a second:
//! every routed source and every bus, with true peak and BS.1770 loudness on the master,
//! which the mixer thread measures on each block it mixes ([`MasterMeters`]).
//!
//! Each source's `follow` is applied as its input's activity gain, computed from the
//! scene on program (`crate::activity`), so a `when_shown` source is heard only while
//! the Program mix shows it. The engine syncs again when program changes.
//!
//! The audio sources' inputs are opened and closed by the program stage
//! (`crate::audio_sources`, which opens none for a source whose audio is off, and runs
//! each source's filter chain); this module only sets them up.
//!
//! The master stage's audio chain runs on the mixer's master chain, so on every bus that
//! feeds an output (ADR-0027, docs/adr/0027-program-master-and-output-chains.md). An
//! output that starts with an audio chain of its own gets an output tap on the master
//! bus, which runs that chain after the master's; the output encodes its audio from the
//! tap instead of sharing the master bus's encode. Each sync brings both chains in line
//! with the collection, so their filters change live.

use crate::activity::activity_gains;
use crate::filters::{Chain, chain_of};
use jstream_audio::meter::{LoudnessMeter, TruePeakMeter};
use jstream_audio::{
    BusControl, InputControl, MeterReading, MixerControl, OutputTapControl, OutputTapId,
};
use jstream_graph::State;
use jstream_protocol::{
    BusId, BusMeter, Finite32, Loudness, OutputId, SceneId, SourceId, SourceMeter,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// The program mixer's controls, while it runs.
#[derive(Default)]
pub(crate) struct AudioRouting {
    attached: Option<Attached>,
    /// Numbers each attach, so a program that stopped detaches only its own mixer.
    attaches: u64,
}

struct Attached {
    generation: u64,
    control: MixerControl,
    master: BusControl,
    /// The mixer bus of each of the collection's other buses.
    buses: BTreeMap<BusId, BusControl>,
    /// Mixer buses whose removal the mixer refused (its command queue was full): each
    /// sync tries again.
    removing: Vec<BusControl>,
    meters: Arc<MasterMeters>,
    /// The master stage's audio chain, on the mixer's master chain.
    master_chain: Chain,
    /// The output tap of each running output with its own audio chain.
    taps: BTreeMap<OutputId, Tap>,
    /// Output taps whose removal the mixer refused: each sync tries again.
    removing_taps: Vec<OutputTapControl>,
}

/// One output's tap and the chain it runs.
struct Tap {
    control: OutputTapControl,
    chain: Chain,
}

/// What the program stage hands the engine when its mixer opens.
pub(crate) struct MixerHandles {
    pub(crate) control: MixerControl,
    pub(crate) master: BusControl,
    pub(crate) meters: Arc<MasterMeters>,
}

impl AudioRouting {
    /// Starts routing into `mixer`, set up as `state` says with `program` on the
    /// Program mix, with the audio sources' mixer `inputs`. Returns the attach's number,
    /// for [`detach`](Self::detach).
    pub(crate) fn attach(
        &mut self,
        mixer: MixerHandles,
        state: &State,
        program: Option<&SceneId>,
        inputs: &BTreeMap<SourceId, InputControl>,
    ) -> u64 {
        self.attaches = self.attaches.wrapping_add(1);
        self.attached = Some(Attached {
            generation: self.attaches,
            control: mixer.control,
            master: mixer.master,
            buses: BTreeMap::new(),
            removing: Vec::new(),
            meters: mixer.meters,
            master_chain: Chain::default(),
            taps: BTreeMap::new(),
            removing_taps: Vec::new(),
        });
        self.sync(state, program, inputs);
        self.attaches
    }

    /// Stops routing into the mixer attach `generation` named, if it is still the one.
    pub(crate) fn detach(&mut self, generation: u64) {
        if self
            .attached
            .as_ref()
            .is_some_and(|a| a.generation == generation)
        {
            self.attached = None;
        }
    }

    /// Adds an output tap on the master bus for `output_id`, running its audio chain as
    /// `state` says, for an output that encodes its own audio (ADR-0027). The engine
    /// keeps the tap's chain in line on every sync, until
    /// [`remove_output_tap`](Self::remove_output_tap).
    ///
    /// # Errors
    /// When no mixer runs, or the mixer refuses the tap.
    pub(crate) fn add_output_tap(
        &mut self,
        output_id: &OutputId,
        state: &State,
    ) -> Result<OutputTapControl, String> {
        let attached = self
            .attached
            .as_mut()
            .ok_or("the program mixer is not running")?;
        let control = attached
            .control
            .add_output_tap(attached.master.id())
            .map_err(|err| format!("output tap: {err}"))?;
        let mut chain = Chain::default();
        if let Some(output) = state.output(output_id) {
            for reason in chain.apply(&control, &chain_of(output.filters())) {
                tracing::warn!(output_id = %output_id, reason, "output filter bypassed");
            }
        }
        if let Some(old) = attached.taps.insert(
            output_id.clone(),
            Tap {
                control: control.clone(),
                chain,
            },
        ) && !old.control.remove()
        {
            attached.removing_taps.push(old.control);
        }
        Ok(control)
    }

    /// Removes `output_id`'s tap `tap`, once its output has stopped. A tap of an earlier
    /// start, or of a mixer that has stopped, is left alone.
    pub(crate) fn remove_output_tap(&mut self, output_id: &OutputId, tap: OutputTapId) {
        let Some(attached) = &mut self.attached else {
            return;
        };
        if attached
            .taps
            .get(output_id)
            .is_some_and(|own| own.control.id() == tap)
            && let Some(own) = attached.taps.remove(output_id)
            && !own.control.remove()
        {
            attached.removing_taps.push(own.control);
        }
    }

    /// Brings the mixer in line with `state`: adds and removes buses, sets every fader,
    /// mute and send, those of the audio sources' `inputs` (which the program stage opens
    /// and closes, `crate::audio_sources`) included, each input's activity gain for
    /// `program` on the Program mix, and the master's and the output taps' filter
    /// chains. Does nothing while no mixer runs.
    pub(crate) fn sync(
        &mut self,
        state: &State,
        program: Option<&SceneId>,
        inputs: &BTreeMap<SourceId, InputControl>,
    ) {
        let Some(attached) = &mut self.attached else {
            return;
        };
        attached.removing.retain(|bus| !bus.remove());
        attached.removing_taps.retain(|tap| !tap.remove());
        for reason in attached
            .master_chain
            .apply(&attached.control.master(), &chain_of(state.master()))
        {
            tracing::warn!(reason, "master filter bypassed");
        }
        for (output_id, tap) in &mut attached.taps {
            // The output may be gone while its failed pipeline drains; its release
            // removes the tap.
            let Some(output) = state.output(output_id) else {
                continue;
            };
            for reason in tap.chain.apply(&tap.control, &chain_of(output.filters())) {
                tracing::warn!(output_id = %output_id, reason, "output filter bypassed");
            }
        }
        let gone: Vec<BusId> = attached
            .buses
            .keys()
            .filter(|id| state.bus(id).is_none())
            .cloned()
            .collect();
        for id in gone {
            if let Some(bus) = attached.buses.remove(&id)
                && !bus.remove()
            {
                attached.removing.push(bus);
            }
        }
        for bus in state.buses() {
            let control = if bus.id().is_master() {
                &attached.master
            } else {
                match attached.buses.entry(bus.id().clone()) {
                    std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        match attached.control.add_bus() {
                            Ok(control) => entry.insert(control),
                            Err(err) => {
                                tracing::warn!(bus_id = %bus.id(), error = %err, "mixer bus not added");
                                continue;
                            }
                        }
                    }
                }
            };
            control.set_gain_db(bus.gain_db().get());
            control.set_muted(bus.muted());
        }
        let activity = activity_gains(state, program);
        // An input whose source went, or has no audio now, stops being routed.
        for (id, input) in inputs {
            let Some(audio) = state.source(id).and_then(|s| s.audio()) else {
                continue;
            };
            // The activity gain and the sends go first: a new input opens muted, and
            // the mixer may run a block between these stores, so it must not become
            // audible before its routing is in place.
            input.set_activity_gain(activity.get(id).copied().unwrap_or(1.0));
            let master = (BusId::master(), attached.master.id());
            let others = attached
                .buses
                .iter()
                .map(|(id, bus)| (id.clone(), bus.id()));
            for (bus_id, mixer_bus) in std::iter::once(master).chain(others) {
                match state.send(id, &bus_id) {
                    Some(_) if audio.muted => input.set_send(mixer_bus, 0.0),
                    Some(gain) => input.set_send_db(mixer_bus, gain.get()),
                    None => {
                        input.remove_send(mixer_bus);
                    }
                }
            }
            input.set_gain_db(audio.gain_db.get());
            // Off, the program stage closes the source's input (`crate::audio_sources`);
            // until it has, the input is silent and unmetered here. A mute keeps the
            // sends but silences them, so the input's post-fader meter still reads
            // (ADR-0028).
            input.set_muted(!audio.enabled);
        }
    }

    /// Every routed source's and bus's levels since the last call, or `None` while no
    /// mixer runs. A source whose audio is off in `state` has no meter.
    pub(crate) fn meters(
        &self,
        state: &State,
        inputs: &BTreeMap<SourceId, InputControl>,
    ) -> Option<(Vec<SourceMeter>, Vec<BusMeter>)> {
        let attached = self.attached.as_ref()?;
        let sources = inputs
            .iter()
            .filter(|(id, _)| {
                state
                    .source(id)
                    .and_then(|s| s.audio())
                    .is_some_and(|audio| audio.enabled)
            })
            .map(|(id, input)| {
                let reading = input.take_meter();
                SourceMeter {
                    source_id: id.clone(),
                    peak: levels(reading.peak),
                    rms: levels(reading.rms),
                }
            })
            .collect();
        let master = bus_meter(BusId::master(), attached.master.take_meter());
        let master = BusMeter {
            true_peak: Some(levels(attached.meters.take_true_peak())),
            loudness: Some(attached.meters.loudness()),
            ..master
        };
        let buses = std::iter::once(master)
            .chain(
                attached
                    .buses
                    .iter()
                    .map(|(id, bus)| bus_meter(id.clone(), bus.take_meter())),
            )
            .collect();
        Some((sources, buses))
    }

    /// Restarts the master's integrated loudness (`audio.reset_loudness`). Does nothing
    /// while no mixer runs.
    pub(crate) fn reset_loudness(&self) {
        if let Some(attached) = &self.attached {
            attached.meters.request_reset();
        }
    }
}

fn bus_meter(bus_id: BusId, reading: MeterReading) -> BusMeter {
    BusMeter {
        bus_id,
        peak: levels(reading.peak),
        rms: levels(reading.rms),
        true_peak: None,
        loudness: None,
    }
}

fn levels(levels: [f32; 2]) -> [Finite32; 2] {
    levels.map(|level| Finite32::new(level).unwrap_or(Finite32::ZERO))
}

/// The master bus's true peak and loudness. The mixer thread measures each block it
/// mixes and publishes here; the engine reads it for `meters.tick`. Atomics only, so
/// neither side waits on the other (R-RT-003).
#[derive(Debug, Default)]
pub(crate) struct MasterMeters {
    /// The held true peak per channel, as `f32` bits: non-negative finite values order
    /// like their bits, so holding is a `fetch_max`.
    true_peak: [AtomicU32; 2],
    /// Momentary, short-term and integrated loudness, as `f64` bits; NaN for none.
    loudness: [AtomicU64; 3],
    reset: AtomicBool,
}

impl MasterMeters {
    fn take_true_peak(&self) -> [f32; 2] {
        self.true_peak
            .each_ref()
            .map(|peak| f32::from_bits(peak.swap(0, Ordering::Relaxed)))
    }

    fn loudness(&self) -> Loudness {
        let [momentary, short_term, integrated] = self.loudness.each_ref().map(|value| {
            let lufs = f64::from_bits(value.load(Ordering::Relaxed));
            #[expect(
                clippy::cast_possible_truncation,
                reason = "LUFS readings sit well inside f32"
            )]
            Finite32::new(lufs as f32)
        });
        Loudness {
            momentary,
            short_term,
            integrated,
        }
    }

    fn request_reset(&self) {
        self.reset.store(true, Ordering::Relaxed);
    }
}

/// The mixer thread's half of [`MasterMeters`]: the meters themselves, run on every
/// master block. Built before the thread starts, since a loudness meter allocates.
pub(crate) struct MasterMetering {
    shared: Arc<MasterMeters>,
    loudness: LoudnessMeter,
    true_peak: TruePeakMeter,
}

impl MasterMetering {
    /// Meters for the master bus at `rate` Hz, and the handle the engine reads.
    pub(crate) fn new(rate: u32) -> Result<(Self, Arc<MasterMeters>), String> {
        let shared = Arc::new(MasterMeters::default());
        for value in &shared.loudness {
            value.store(f64::NAN.to_bits(), Ordering::Relaxed);
        }
        let loudness = LoudnessMeter::new(rate).map_err(|err| format!("loudness meter: {err}"))?;
        Ok((
            Self {
                shared: Arc::clone(&shared),
                loudness,
                true_peak: TruePeakMeter::new(),
            },
            shared,
        ))
    }

    /// Meters one master block (interleaved stereo) and publishes. Never allocates.
    pub(crate) fn process(&mut self, block: &[f32]) {
        if self.shared.reset.swap(false, Ordering::Relaxed) {
            self.loudness.reset_integrated();
        }
        self.loudness.process(block);
        self.true_peak.process(block);
        let peak = self.true_peak.take_peak();
        for (held, value) in self.shared.true_peak.iter().zip(peak) {
            if value.is_finite() {
                held.fetch_max(value.max(0.0).to_bits(), Ordering::Relaxed);
            }
        }
        let readings = [
            self.loudness.momentary(),
            self.loudness.short_term(),
            self.loudness.integrated(),
        ];
        for (value, reading) in self.shared.loudness.iter().zip(readings) {
            value.store(reading.unwrap_or(f64::NAN).to_bits(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioRouting, MasterMetering, MixerHandles};
    use jstream_audio::{Mixer, MixerConfig};
    use jstream_graph::Graph;
    use jstream_media::{AudioFormat, ChannelLayout, SampleFormat};
    use jstream_protocol::{
        AudioFollow, BusId, Edit, Finite32, KindCategory, KindDescriptor, Settings, SourceId,
        SourceKind,
    };
    use std::collections::BTreeMap;

    fn db(v: f32) -> Finite32 {
        Finite32::new(v).expect("finite")
    }

    fn apply(graph: &mut Graph, edits: Vec<Edit>) {
        graph.apply(&edits, None).expect("the edits apply");
    }

    /// A graph with an audio-only kind, a `mic` source of it and a `vod` bus.
    fn graph() -> Graph {
        let kind = KindDescriptor {
            kind_id: "tone".into(),
            category: KindCategory::Source,
            name: "Tone".into(),
            version: 1,
            video: false,
            audio: true,
            params: vec![],
        };
        let mut graph = Graph::new().with_kinds(vec![kind]);
        apply(
            &mut graph,
            vec![
                Edit::SourceCreate {
                    source_id: SourceId::new("mic"),
                    name: "Mic".into(),
                    kind: SourceKind::Builtin {
                        kind: "tone".into(),
                    },
                    settings: Settings::new(),
                    duplicated_from: None,
                },
                Edit::BusCreate {
                    bus_id: BusId::new("vod"),
                    name: "VOD".into(),
                },
            ],
        );
        graph
    }

    #[test]
    fn the_mixer_follows_buses_faders_and_sends() {
        let mut mixer = Mixer::new(MixerConfig::default()).expect("mixer");
        let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("format");
        let (_producer, input) = mixer.add_input(format).expect("input");
        let (_metering, meters) = MasterMetering::new(48_000).expect("meters");
        let mut graph = graph();
        let inputs = BTreeMap::from([(SourceId::new("mic"), input.clone())]);
        let mut routing = AudioRouting::default();
        let generation = routing.attach(
            MixerHandles {
                control: mixer.control(),
                master: mixer.bus(),
                meters,
            },
            graph.state(),
            None,
            &inputs,
        );
        let attached = routing.attached.as_ref().expect("attached");
        let vod = attached.buses[&BusId::new("vod")].id();
        // An audio input follows `always`.
        assert_eq!(input.activity_gain(), 1.0);
        // A new source sends to the master at 0 dB, and nowhere else.
        assert_eq!(input.send(jstream_audio::BusId::MASTER), Some(1.0));
        assert_eq!(input.send(vod), None);

        apply(
            &mut graph,
            vec![
                Edit::SendSet {
                    source_id: SourceId::new("mic"),
                    bus_id: BusId::new("vod"),
                    gain_db: db(-6.0),
                },
                Edit::SendRemove {
                    source_id: SourceId::new("mic"),
                    bus_id: BusId::master(),
                },
                Edit::SourceSetAudio {
                    source_id: SourceId::new("mic"),
                    enabled: None,
                    follow: Some(AudioFollow::WhenShown),
                    gain_db: Some(db(6.0)),
                    muted: Some(true),
                },
                Edit::BusSetFader {
                    bus_id: BusId::master(),
                    gain_db: Some(db(-20.0)),
                    muted: None,
                },
            ],
        );
        routing.sync(graph.state(), None, &inputs);
        assert_eq!(input.send(jstream_audio::BusId::MASTER), None);
        // Muted: the send is kept but silent, and the input itself still plays to its
        // meter.
        assert_eq!(input.send(vod), Some(0.0));
        // Following `when_shown` with no scene on program, it is not heard.
        assert_eq!(input.activity_gain(), 0.0);
        assert!((input.gain() - 1.995).abs() < 1e-3, "{}", input.gain());
        assert!(!input.muted());
        assert!((mixer.bus().gain() - 0.1).abs() < 1e-4);
        let (sources, _) = routing.meters(graph.state(), &inputs).expect("meters");
        assert_eq!(sources.len(), 1, "a muted source is metered");

        apply(
            &mut graph,
            vec![Edit::SourceSetAudio {
                source_id: SourceId::new("mic"),
                enabled: None,
                follow: None,
                gain_db: None,
                muted: Some(false),
            }],
        );
        routing.sync(graph.state(), None, &inputs);
        let send = input.send(vod).expect("a send to vod");
        assert!((send - 0.501).abs() < 1e-3, "{send}");

        // Off mutes the input and drops its meter; deleting the bus removes its mixer bus.
        apply(
            &mut graph,
            vec![
                Edit::SourceSetAudio {
                    source_id: SourceId::new("mic"),
                    enabled: Some(false),
                    follow: None,
                    gain_db: None,
                    muted: Some(false),
                },
                Edit::BusDelete {
                    bus_id: BusId::new("vod"),
                },
            ],
        );
        routing.sync(graph.state(), None, &inputs);
        assert!(input.muted());
        let attached = routing.attached.as_ref().expect("attached");
        assert!(attached.buses.is_empty());
        let (sources, buses) = routing
            .meters(graph.state(), &inputs)
            .expect("meters while attached");
        assert!(
            sources.is_empty(),
            "a source whose audio is off has no meter"
        );
        assert_eq!(buses.len(), 1);
        assert!(buses[0].bus_id.is_master());
        assert!(buses[0].loudness.is_some() && buses[0].true_peak.is_some());

        routing.detach(generation.wrapping_add(1));
        assert!(
            routing.meters(graph.state(), &inputs).is_some(),
            "another attach's detach is ignored"
        );
        routing.detach(generation);
        assert!(routing.meters(graph.state(), &inputs).is_none());
    }

    #[test]
    fn master_metering_publishes_true_peak_and_loudness_and_resets() {
        let (mut metering, meters) = MasterMetering::new(48_000).expect("meters");
        // A second of a full-scale-ish 1 kHz tone.
        let block: Vec<f32> = (0..48_000)
            .flat_map(|n| {
                let s = 0.5 * (std::f32::consts::TAU * 1_000.0 * n as f32 / 48_000.0).sin();
                [s, s]
            })
            .collect();
        metering.process(&block);
        let peak = meters.take_true_peak();
        assert!(peak[0] > 0.49 && peak[0] < 0.52, "{peak:?}");
        assert_eq!(
            meters.take_true_peak(),
            [0.0, 0.0],
            "taking restarts the peak"
        );
        let loudness = meters.loudness();
        assert!(loudness.momentary.is_some());
        assert!(loudness.integrated.is_some());
        meters.request_reset();
        metering.process(&block[..960]);
        assert_eq!(meters.loudness().integrated, None);
    }
}
