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
//! The `waveform` and `spectrum` sources that draw a bus (#362) or another source
//! (#459) get its scope from the engine's [`AudioScopes`]. Each sync keeps a scope for
//! every bus a source's settings refer to and every source a scope source's
//! `audio_source` names, drops the others, and hands the mixer thread the list of what
//! feeds each ([`ScopeTaps`], through `arc-swap`, so the thread never waits on the
//! engine): a bus's mixer bus, or a drawn source's mixer input, whose block is that
//! source after its fader and activity gain. A drawn source that is muted, whose audio
//! is off, or that has no mixer input now (its stream isn't open) is fed silence, so
//! it draws what the audience hears from it.
//!
//! Each bus with a monitor (ADR-0028's D22) is played on its output device while the
//! mixer runs. Each sync hands the program stage's audio thread the monitors to play,
//! each with the mixer bus that feeds it (`crate::audio_sources::Wanted::set_monitors`);
//! that thread opens the device off the collection lock and hands the monitor's feed to
//! the mixer thread as a [`MonitorRoute`], which pushes each block the monitor hears
//! into it, before or after the bus's filters.
//!
//! The audio sources' inputs are opened and closed by the program stage
//! (`crate::audio_sources`, which opens none for a source whose audio is off, and runs
//! each source's filter chain); this module only sets them up.
//!
//! The master stage's audio chain runs on the mixer's master chain, so on every bus that
//! feeds an output (ADR-0080, docs/adr/0080-program-master-and-output-chains-v2.md). An
//! output that starts with an audio chain of its own, or that carries a bus other than
//! the master (`output.set_audio_buses`), gets an output tap on its bus, which runs its
//! chain after the master's; the output encodes its audio from the tap instead of
//! sharing the master bus's encode. The tap stays on the bus the output started with.
//! Each sync brings both chains in line with the collection, so their filters change
//! live.
//!
//! A CLAP filter on either (ADR-0035) runs in a plugin host process, which the program
//! stage's audio thread starts and supervises off the collection lock
//! (`crate::audio_sources::Wanted::set_routed_chains`). Each sync names both chains to
//! that thread and puts each CLAP filter on its host's block once it is ready; until
//! then the filter is bypassed. The next `meters.tick` syncs again when a block changes.
//!
//! A master CLAP filter runs one host per bus that feeds an output (ADR-0086,
//! docs/adr/0086-master-clap-filter-one-host-per-output-bus.md): the master bus's copy
//! of the filter runs on the host it was added with, and each other bus an output tap
//! reads gets hosts of its own for the master chain's CLAP filters, named to the same
//! thread. Each sync puts every such bus's copy on its own host's block once both that
//! block and the master bus's filter are ready; until then the copy passes the bus's
//! dry signal at the filter's latency, as it does again once the bus feeds no output.
//! Every host gets the filter's settings, and each keeps its own state.

use crate::activity::activity_gains;
use crate::audio_sources::{RoutedChains, Wanted, WantedMonitor};
use crate::filters::{Chain, Clap, chain_of, same_block};
use arc_swap::ArcSwap;
use jstream_audio::meter::{LoudnessMeter, TruePeakMeter};
use jstream_audio::{
    BusControl, InputControl, MeterReading, MixerControl, MonitorFeed, OutputTapControl,
    OutputTapId,
};
use jstream_graph::State;
use jstream_platform::HostBlock;
use jstream_protocol::{
    BusId, BusMeter, FilterId, Finite32, Loudness, OutputId, Reference, SceneId, SourceId,
    SourceKind, SourceMeter,
};
use jstream_sources::{AudioScope, AudioScopes};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

// The mixer has a bus slot for every bus a collection may hold, and no more: the
// engine's limit and the protocol's are one number (#372).
const _: () = assert!(jstream_audio::MAX_BUSES == jstream_protocol::MAX_BUSES);

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
    scopes: Arc<AudioScopes>,
    taps: Arc<ScopeTaps>,
    /// The master stage's audio chain, on the mixer's master chain.
    master_chain: Chain,
    /// The output tap of each running output with its own audio chain or a bus other
    /// than the master.
    output_taps: BTreeMap<OutputId, Tap>,
    /// Each bus other than the master that feeds an output, by bus: the master chain's
    /// CLAP filters its copies run on hosts of their own (ADR-0086).
    bus_hosts: BTreeMap<BusId, BusHosts>,
    /// Output taps whose removal the mixer refused, by output: each sync tries again.
    removing_taps: Vec<(OutputId, OutputTapControl)>,
    /// The chains last named to the program stage's audio thread, which runs their
    /// CLAP filters' hosts.
    routed: RoutedChains,
    /// Where the program stage's audio thread learns which monitors to play.
    wanted: Arc<Wanted>,
}

/// One output's tap, the bus it reads and the chain it runs.
struct Tap {
    control: OutputTapControl,
    bus: BusId,
    chain: Chain,
}

/// The master CLAP filters one bus's copies run on hosts of their own (ADR-0086).
struct BusHosts {
    /// The mixer bus they are on.
    mixer: jstream_audio::BusId,
    /// The block each copy runs on, by the master filter's mixer id.
    hosted: HashMap<jstream_audio::FilterId, HostBlock>,
    /// The block each copy refused, so it isn't tried again until the block changes.
    refused: HashMap<jstream_audio::FilterId, HostBlock>,
}

/// What the program stage hands the engine when its mixer opens.
pub(crate) struct MixerHandles {
    pub(crate) control: MixerControl,
    pub(crate) master: BusControl,
    pub(crate) meters: Arc<MasterMeters>,
    /// The engine's scopes, whose bus scopes the mixer thread feeds.
    pub(crate) scopes: Arc<AudioScopes>,
    /// Where the mixer thread reads which mixer bus feeds which scope.
    pub(crate) taps: Arc<ScopeTaps>,
    /// The program stage's audio thread's wanted set, which opens the monitors.
    pub(crate) wanted: Arc<Wanted>,
}

/// One monitor's feed on the mixer thread: after each block, what `bus` sounds like at
/// `tap` goes into `feed`. The program stage's audio thread opens the device and sends
/// the route over; it sets `closed` when it closes the monitor, and the mixer thread
/// then hands the route to the collector.
#[derive(Debug)]
pub(crate) struct MonitorRoute {
    pub(crate) bus: jstream_audio::BusId,
    pub(crate) tap: jstream_audio::MonitorTap,
    pub(crate) feed: MonitorFeed,
    pub(crate) closed: Arc<std::sync::atomic::AtomicBool>,
}

/// One bus or source scope the mixer thread feeds: after each block, the block `feed`
/// names goes into `scope`.
#[derive(Debug)]
pub(crate) struct ScopeTap {
    pub(crate) feed: TapFeed,
    pub(crate) scope: Arc<AudioScope>,
}

/// Where a [`ScopeTap`]'s blocks come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TapFeed {
    /// A mixer bus, after its fader ([`jstream_audio::Mixer::bus_block`]).
    Bus(jstream_audio::BusId),
    /// A source's mixer input, after its fader, mute and activity gain, before its
    /// sends ([`jstream_audio::Mixer::input_block`]).
    Input(jstream_audio::InputId),
    /// Silence: a drawn source that is muted, off, or has no mixer input.
    Silence,
}

/// The bus scopes the mixer thread feeds, handed over as an immutable list through
/// `arc-swap`, as the render snapshot is: the engine stores a new list under the
/// collection lock, and the mixer thread loads it without any lock, keeps a counted
/// reference, and hands the list it lets go of to the collector (R-RT-003).
#[derive(Debug, Default)]
pub(crate) struct ScopeTaps {
    current: ArcSwap<Vec<ScopeTap>>,
}

impl ScopeTaps {
    /// Stores `taps`, unless they feed exactly the scopes the current list feeds.
    fn publish(&self, taps: Vec<ScopeTap>) {
        let current = self.current.load();
        let same = current.len() == taps.len()
            && current
                .iter()
                .zip(&taps)
                .all(|(a, b)| a.feed == b.feed && Arc::ptr_eq(&a.scope, &b.scope));
        if !same {
            self.current.store(Arc::new(taps));
        }
    }

    /// The current list.
    #[cfg(test)]
    pub(crate) fn current(&self) -> Arc<Vec<ScopeTap>> {
        self.current.load_full()
    }

    /// The current list, for the mixer thread's [`TapReader`].
    pub(crate) fn reader(self: &Arc<Self>) -> TapReader {
        TapReader {
            held: self.current.load_full(),
            taps: Arc::clone(self),
        }
    }
}

/// The mixer thread's handle on [`ScopeTaps`]: the list it holds.
pub(crate) struct TapReader {
    taps: Arc<ScopeTaps>,
    held: Arc<Vec<ScopeTap>>,
}

impl TapReader {
    /// The current list. When the engine stored a new one since the last call, this
    /// adopts it and returns the one it held, for the caller to hand to the collector;
    /// otherwise it only compares a pointer. Never locks, waits or allocates.
    pub(crate) fn adopt(&mut self) -> (&[ScopeTap], Option<Arc<Vec<ScopeTap>>>) {
        let guard = self.taps.current.load();
        let mut old = None;
        if !Arc::ptr_eq(&self.held, &guard) {
            // The guard becomes a counted reference that `held` keeps, so it is never the
            // last one dropped here.
            old = Some(std::mem::replace(
                &mut self.held,
                arc_swap::Guard::into_inner(guard),
            ));
        }
        (&self.held, old)
    }

    /// Lets go of the list: the caller hands it to the collector.
    pub(crate) fn into_held(self) -> Arc<Vec<ScopeTap>> {
        self.held
    }
}

/// Keeps `scopes`' bus and source scopes in line with what `state`'s sources draw. The
/// live sources call it before they queue builds, and each sync calls it again before
/// it hands the mixer thread what feeds each scope.
pub(crate) fn retain_scopes(scopes: &AudioScopes, state: &State) {
    scopes.retain(&drawn_buses(state));
    scopes.retain_sources(&drawn_sources(state));
}

/// Every bus a source's settings refer to: the buses whose scopes are drawn.
fn drawn_buses(state: &State) -> BTreeSet<BusId> {
    state
        .sources()
        .flat_map(|source| source.settings().references())
        .filter_map(|(_, reference)| match reference {
            Reference::Bus { bus_id } => Some(bus_id),
            _ => None,
        })
        .collect()
}

/// Every source a scope source's `audio_source` draws (#459).
fn drawn_sources(state: &State) -> BTreeSet<SourceId> {
    state
        .sources()
        .filter_map(|source| match source.kind() {
            SourceKind::Builtin { kind } => jstream_sources::drawn_source(kind, source.settings()),
            _ => None,
        })
        .collect()
}

impl Attached {
    /// The buses but the master that an output tap reads and the mixer still has: a
    /// bus gone from the mixer needs no hosts.
    fn tapped_buses(&self) -> BTreeSet<BusId> {
        self.output_taps
            .values()
            .filter(|tap| self.buses.contains_key(&tap.bus))
            .map(|tap| tap.bus.clone())
            .collect()
    }

    /// Names the master chain and each tapped output's chain to the program stage's
    /// audio thread, which runs their CLAP filters' hosts.
    fn name_routed(&mut self, state: &State) {
        self.routed = RoutedChains {
            master: chain_of(state.master()),
            outputs: self
                .output_taps
                .keys()
                .filter_map(|id| Some((id.clone(), chain_of(state.output(id)?.filters()))))
                .collect(),
            buses: self.tapped_buses(),
        };
        self.wanted.set_routed_chains(self.routed.clone());
    }

    /// Puts each bus that feeds an output, but the master, on its own hosts for the
    /// master chain's CLAP filters, once both the host's block in `blocks` and the master
    /// bus's filter are ready, and back on the stand-in when it no longer feeds one or
    /// its host goes (ADR-0086).
    fn sync_bus_hosts(&mut self, blocks: &BTreeMap<BusId, BTreeMap<FilterId, HostBlock>>) {
        let fed: BTreeMap<BusId, jstream_audio::BusId> = self
            .routed
            .buses
            .iter()
            .filter_map(|id| Some((id.clone(), self.buses.get(id)?.id())))
            .collect();
        let clap: Vec<(FilterId, jstream_audio::FilterControl)> = self
            .master_chain
            .clap_controls()
            .map(|(id, control)| (id.clone(), control.clone()))
            .collect();
        // A bus that no longer feeds an output, or is a new mixer bus now, goes back to
        // the stand-in.
        self.bus_hosts.retain(|bus_id, hosts| {
            if fed.get(bus_id) == Some(&hosts.mixer) {
                return true;
            }
            for (_, control) in &clap {
                if hosts.hosted.contains_key(&control.id()) {
                    let _ = control.set_bus_host(hosts.mixer, None);
                }
            }
            false
        });
        // A removed filter the mixer's queue hasn't taken yet may still run a bus's
        // block: wait for it.
        if !self.master_chain.settled() {
            return;
        }
        let none = BTreeMap::new();
        for (bus_id, mixer) in fed {
            let hosts = self.bus_hosts.entry(bus_id.clone()).or_insert(BusHosts {
                mixer,
                hosted: HashMap::new(),
                refused: HashMap::new(),
            });
            // Copies of filters the chain rebuilt or removed went with them.
            let live: HashSet<jstream_audio::FilterId> =
                clap.iter().map(|(_, control)| control.id()).collect();
            hosts.hosted.retain(|id, _| live.contains(id));
            hosts.refused.retain(|id, _| live.contains(id));
            let ready = blocks.get(&bus_id).unwrap_or(&none);
            for (filter_id, control) in &clap {
                let want = ready.get(filter_id);
                let id = control.id();
                if same_block(hosts.hosted.get(&id), want)
                    || (want.is_some() && same_block(hosts.refused.get(&id), want))
                {
                    continue;
                }
                match control.set_bus_host(mixer, want.cloned()) {
                    Ok(()) => {
                        hosts.refused.remove(&id);
                        match want {
                            Some(block) => hosts.hosted.insert(id, Arc::clone(block)),
                            None => hosts.hosted.remove(&id),
                        };
                    }
                    Err(err) => {
                        tracing::warn!(
                            bus_id = %bus_id,
                            filter_id = %filter_id,
                            error = %err,
                            "master filter keeps the bus's dry signal"
                        );
                        if let Some(block) = want {
                            hosts.refused.insert(id, Arc::clone(block));
                        }
                    }
                }
            }
        }
    }

    /// The ready CLAP blocks of `output_id`'s tap chain: none while an older tap of
    /// the output that the mixer still runs may hold them.
    fn tap_blocks(
        &self,
        output_id: &OutputId,
    ) -> BTreeMap<jstream_protocol::FilterId, jstream_platform::HostBlock> {
        if self.removing_taps.iter().any(|(id, _)| id == output_id) {
            return BTreeMap::new();
        }
        self.wanted
            .routed_blocks()
            .outputs
            .remove(output_id)
            .unwrap_or_default()
    }
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
            output_taps: BTreeMap::new(),
            bus_hosts: BTreeMap::new(),
            removing_taps: Vec::new(),
            routed: RoutedChains::default(),
            scopes: mixer.scopes,
            taps: mixer.taps,
            wanted: mixer.wanted,
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
            && let Some(attached) = self.attached.take()
        {
            // Its mixer buses go with it: the next program's names its own.
            attached.wanted.set_monitors(BTreeMap::new());
            attached.wanted.set_routed_chains(RoutedChains::default());
        }
    }

    /// Adds an output tap for `output_id` on the bus it carries, running its audio chain
    /// as `state` says, for an output that encodes its own audio (ADR-0080). The engine
    /// keeps the tap's chain in line on every sync, until
    /// [`remove_output_tap`](Self::remove_output_tap), and a bus other than the master
    /// that a tap reads gets hosts of its own for the master chain's CLAP filters
    /// (ADR-0086).
    ///
    /// # Errors
    /// When no mixer runs, the mixer has no bus for the output's bus yet, or the mixer
    /// refuses the tap.
    pub(crate) fn add_output_tap(
        &mut self,
        output_id: &OutputId,
        state: &State,
    ) -> Result<OutputTapControl, String> {
        let attached = self
            .attached
            .as_mut()
            .ok_or("the program mixer is not running")?;
        let bus = state
            .output(output_id)
            .map_or_else(BusId::master, |output| output.audio_bus().clone());
        let mixer_bus = if bus.is_master() {
            attached.master.id()
        } else {
            attached
                .buses
                .get(&bus)
                .ok_or_else(|| format!("output tap: the mixer has no bus `{bus}`"))?
                .id()
        };
        let control = attached
            .control
            .add_output_tap(mixer_bus)
            .map_err(|err| format!("output tap: {err}"))?;
        // Queued before the new tap's filters, so a CLAP filter's block never feeds two
        // taps at once.
        if let Some(old) = attached.output_taps.remove(output_id)
            && !old.control.remove()
        {
            attached
                .removing_taps
                .push((output_id.clone(), old.control));
        }
        let mut chain = Chain::default();
        if let Some(output) = state.output(output_id) {
            let blocks = attached.tap_blocks(output_id);
            for reason in chain.apply(
                &control,
                &chain_of(output.filters()),
                Clap::Blocks(&blocks),
                Instant::now(),
            ) {
                tracing::warn!(output_id = %output_id, reason, "output filter not applied");
            }
        }
        attached.output_taps.insert(
            output_id.clone(),
            Tap {
                control: control.clone(),
                bus,
                chain,
            },
        );
        attached.name_routed(state);
        Ok(control)
    }

    /// How many CLAP filters run on a host's block in the master chain and in each
    /// output tap's chain, for tests.
    #[cfg(test)]
    pub(crate) fn hosted(&self) -> (usize, BTreeMap<OutputId, usize>) {
        self.attached.as_ref().map_or_else(Default::default, |a| {
            (
                a.master_chain.hosted(),
                a.output_taps
                    .iter()
                    .map(|(id, tap)| (id.clone(), tap.chain.hosted()))
                    .collect(),
            )
        })
    }

    /// How many master CLAP filters each bus other than the master runs on a host of
    /// its own, and the bus each output tap reads, for tests.
    #[cfg(test)]
    pub(crate) fn bus_hosted(&self) -> (BTreeMap<BusId, usize>, BTreeMap<OutputId, BusId>) {
        self.attached.as_ref().map_or_else(Default::default, |a| {
            (
                a.bus_hosts
                    .iter()
                    .map(|(id, hosts)| (id.clone(), hosts.hosted.len()))
                    .collect(),
                a.output_taps
                    .iter()
                    .map(|(id, tap)| (id.clone(), tap.bus.clone()))
                    .collect(),
            )
        })
    }

    /// Removes `output_id`'s tap `tap`, once its output has stopped. A tap of an earlier
    /// start, or of a mixer that has stopped, is left alone.
    pub(crate) fn remove_output_tap(&mut self, output_id: &OutputId, tap: OutputTapId) {
        let Some(attached) = &mut self.attached else {
            return;
        };
        if attached
            .output_taps
            .get(output_id)
            .is_some_and(|own| own.control.id() == tap)
            && let Some(own) = attached.output_taps.remove(output_id)
        {
            if !own.control.remove() {
                attached
                    .removing_taps
                    .push((output_id.clone(), own.control));
            }
            // Its CLAP filters' hosts stop, and its bus's own master hosts once no other
            // tap reads it; the next sync puts that bus's copies back on the stand-in.
            attached.routed.outputs.remove(output_id);
            attached.routed.buses = attached.tapped_buses();
            attached.wanted.set_routed_chains(attached.routed.clone());
        }
    }

    /// Brings the mixer in line with `state`: adds and removes buses, sets every fader,
    /// mute and send, those of the audio sources' `inputs` (which the program stage opens
    /// and closes, `crate::audio_sources`) included, each input's activity gain for
    /// `program` on the Program mix, and the master's and the output taps' filter
    /// chains, and points the mixer thread at the buses sources draw. Does nothing while
    /// no mixer runs.
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
        attached.removing_taps.retain(|(_, tap)| !tap.remove());
        attached.name_routed(state);
        let blocks = attached.wanted.routed_blocks();
        for reason in attached.master_chain.apply(
            &attached.control.master(),
            &attached.routed.master,
            Clap::Blocks(&blocks.master),
            Instant::now(),
        ) {
            tracing::warn!(reason, "master filter not applied");
        }
        let none = BTreeMap::new();
        for (output_id, tap) in &mut attached.output_taps {
            // The output may be gone while its failed pipeline drains; its release
            // removes the tap.
            let Some(wanted) = attached.routed.outputs.get(output_id) else {
                continue;
            };
            // An older tap of this output that the mixer still runs may hold the
            // blocks: they wait for it to go.
            let blocks = if attached.removing_taps.iter().any(|(id, _)| id == output_id) {
                &none
            } else {
                blocks.outputs.get(output_id).unwrap_or(&none)
            };
            for reason in
                tap.chain
                    .apply(&tap.control, wanted, Clap::Blocks(blocks), Instant::now())
            {
                tracing::warn!(output_id = %output_id, reason, "output filter not applied");
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
        // Each bus that feeds an output on its own master hosts, now that the mixer
        // holds every bus.
        attached.sync_bus_hosts(&blocks.buses);
        // The monitors, each on the mixer bus its bus has now.
        let monitors = state
            .buses()
            .filter_map(|bus| {
                let monitor = bus.monitor()?;
                let mixer_bus = if bus.id().is_master() {
                    attached.master.id()
                } else {
                    attached.buses.get(bus.id())?.id()
                };
                Some((
                    bus.id().clone(),
                    WantedMonitor {
                        bus: mixer_bus,
                        monitor: monitor.clone(),
                    },
                ))
            })
            .collect();
        attached.wanted.set_monitors(monitors);
        // The scopes of the buses sources draw, each fed from its mixer bus once the
        // mixer holds it, and of the sources they draw, each fed from its input.
        retain_scopes(&attached.scopes, state);
        let buses = attached
            .scopes
            .buses()
            .into_iter()
            .filter_map(|(id, scope)| {
                let bus = if id.is_master() {
                    attached.master.id()
                } else {
                    attached.buses.get(&id)?.id()
                };
                Some(ScopeTap {
                    feed: TapFeed::Bus(bus),
                    scope,
                })
            });
        let sources = attached.scopes.sources().into_iter().map(|(id, scope)| {
            let audible = state
                .source(&id)
                .and_then(|s| s.audio())
                .is_some_and(|audio| audio.enabled && !audio.muted);
            let feed = match inputs.get(&id) {
                Some(input) if audible => TapFeed::Input(input.id()),
                _ => TapFeed::Silence,
            };
            ScopeTap { feed, scope }
        });
        attached.taps.publish(buses.chain(sources).collect());
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
            // Muted by its sends, its block still sounds; a compressor keyed on it must
            // hear the mute (ADR-0028).
            input.set_key_silenced(audio.muted);
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
    use super::{AudioRouting, MasterMetering, MixerHandles, ScopeTaps};
    use jstream_audio::{Mixer, MixerConfig};
    use jstream_graph::Graph;
    use jstream_media::{AudioFormat, ChannelLayout, SampleFormat};
    use jstream_protocol::{
        AudioFollow, BusId, Edit, Finite32, KindCategory, KindDescriptor, Settings, SourceId,
        SourceKind,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;

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
            max_outset_px: None,
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
                scopes: Arc::default(),
                taps: Arc::new(ScopeTaps::default()),
                wanted: Arc::default(),
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
        // ... and a compressor keyed on it hears the mute.
        assert!(input.key_silenced());
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
        assert!(!input.key_silenced());

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
