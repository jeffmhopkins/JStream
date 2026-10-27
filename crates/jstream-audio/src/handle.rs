//! Control-plane handles: faders, mutes, sends, filter chains, meters and counters of
//! the inputs and the buses.
//! Every method is a few atomic operations, so the control thread never waits on a
//! real-time thread and vice versa (docs/RULES.md#R-RT-003).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::rt::bus::{BusShared, MASTER_ID};
use crate::rt::chain::{FilterParams, FilterSpec};
use crate::rt::cmd::{ChainTarget, ControlPlane};
use crate::rt::host::HostMemory;
use crate::rt::input::InputShared;
use crate::rt::ring::FrameRing;
use crate::rt::shared::db_to_gain;
use crate::{AudioError, MeterReading};

/// One mixer's input. Opaque: only [`InputControl::id`] produces one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputId(pub(crate) u64);

/// One mixer's bus. Opaque: [`BusControl::id`] produces one, and [`BusId::MASTER`]
/// names the master bus. Ids are never reused within a mixer, so the id of a removed
/// bus never names a later one. Each id is tagged with its mixer: an input, or
/// [`Mixer::bus_block`](crate::Mixer::bus_block), ignores another mixer's bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BusId {
    /// The mixer's buffer slot for the bus.
    pub(crate) slot: usize,
    pub(crate) id: u64,
    /// The mixer's tag; 0 for [`BusId::MASTER`], which names every mixer's master.
    mixer: u64,
}

impl BusId {
    /// The master bus: every mixer has it, and every new input sends to it at 0 dB.
    pub const MASTER: Self = Self {
        slot: 0,
        id: MASTER_ID,
        mixer: 0,
    };

    pub(crate) fn new(slot: usize, id: u64, mixer: u64) -> Self {
        Self { slot, id, mixer }
    }

    /// Whether this names a bus of the mixer tagged `mixer`.
    pub(crate) fn belongs_to(self, mixer: u64) -> bool {
        self.mixer == 0 || self.mixer == mixer
    }
}

/// One filter in a mixer's chains. Opaque: [`FilterControl::id`] produces one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FilterId(u64);

/// Edits and removes one filter in an input's, a bus's, the master's or an output tap's
/// chain. Cheap to clone.
#[derive(Clone, Debug)]
pub struct FilterControl {
    id: u64,
    target: ChainTarget,
    latency: usize,
    plane: Arc<ControlPlane>,
}

impl FilterControl {
    /// This filter's id.
    pub fn id(&self) -> FilterId {
        FilterId(self.id)
    }

    /// The latency the filter adds to its chain, frames (the limiter's look-ahead, a
    /// CLAP filter's pipeline depth plus its plugin's latency; 0 for the others). Fixed
    /// when it was built.
    pub fn latency_frames(&self) -> usize {
        self.latency
    }

    /// Changes the filter's settings. The mixer applies them at the start of its next
    /// block, through the kernel's own setter (a gain change ramps; the others take
    /// effect at once).
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] when `params` is another kind of filter's or out of
    /// range, [`AudioError::Removed`] when the filter (or its input or bus) was removed,
    /// and [`AudioError::CommandQueueFull`] when the queue can't take the edit.
    pub fn set(&self, params: FilterParams) -> Result<(), AudioError> {
        self.plane.edit_filter(self.target, self.id, params)
    }

    /// Removes the filter from its chain; it is freed on the collector thread. False when
    /// it was already removed (or its input or bus was), or the queue can't take it.
    pub fn remove(&self) -> bool {
        self.plane.remove_filter(self.target, self.id)
    }
}

/// Adds `spec` at `index` in `target`'s chain.
fn insert_filter(
    plane: &Arc<ControlPlane>,
    target: ChainTarget,
    index: usize,
    spec: FilterSpec,
) -> Result<FilterControl, AudioError> {
    let (id, latency) = plane.add_filter(target, index, spec)?;
    Ok(FilterControl {
        id,
        target,
        latency,
        plane: Arc::clone(plane),
    })
}

/// Adds the CLAP filter whose host uses `memory` at `index` in `target`'s chain.
fn insert_host_filter(
    plane: &Arc<ControlPlane>,
    target: ChainTarget,
    index: usize,
    memory: HostMemory,
) -> Result<FilterControl, AudioError> {
    let (id, latency) = plane.add_host_filter(target, index, memory)?;
    Ok(FilterControl {
        id,
        target,
        latency,
        plane: Arc::clone(plane),
    })
}

/// Controls and observes one mixer input: the source's filter chain, fader and mute,
/// and its row of the send matrix. Cheap to clone; every clone controls the
/// same input.
///
/// It also holds a reference to everything the input's audio thread and the mixer
/// thread share (its ring and its counters). The mixer and the producer hand their own
/// references to the collector thread, so whichever handle goes last, neither real-time
/// thread frees them (docs/RULES.md#R-RT-003).
#[derive(Clone, Debug)]
pub struct InputControl {
    id: InputId,
    shared: Arc<InputShared>,
    plane: Arc<ControlPlane>,
    /// Never read: it only keeps the ring's last reference off the real-time threads.
    _ring: Arc<FrameRing>,
}

impl InputControl {
    pub(crate) fn new(
        id: InputId,
        shared: Arc<InputShared>,
        ring: Arc<FrameRing>,
        plane: Arc<ControlPlane>,
    ) -> Self {
        Self {
            id,
            shared,
            plane,
            _ring: ring,
        }
    }

    /// This input's id.
    pub fn id(&self) -> InputId {
        self.id
    }

    /// Appends a filter to the input's chain, which runs before its fader. Allocates the
    /// filter here; the mixer installs it at the start of its next block.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] for settings out of range or a chain whose latency
    /// would pass 100 ms, [`AudioError::TooManyFilters`] past 8 filters,
    /// [`AudioError::Removed`] once the input was removed, and
    /// [`AudioError::CommandQueueFull`].
    pub fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError> {
        self.insert_filter(usize::MAX, spec)
    }

    /// [`Self::add_filter`] at `index` in the chain (clamped to its end).
    ///
    /// # Errors
    /// As [`Self::add_filter`].
    pub fn insert_filter(
        &self,
        index: usize,
        spec: FilterSpec,
    ) -> Result<FilterControl, AudioError> {
        insert_filter(&self.plane, ChainTarget::Input(self.id.0), index, spec)
    }

    /// Adds a CLAP filter at `index` in the input's chain (clamped to its end):
    /// the mixer's side of the plugin host that uses `memory`
    /// (docs/adr/0035-clap-plugins-out-of-process.md). The block must be initialised
    /// for this mixer's rate and block size and its host ready. The mixer never waits
    /// on the host; a block that isn't back in time is bypassed and counted in the
    /// block's status. Its latency is the pipeline depth plus the plugin's. Its
    /// parameters travel through the block's parameter ring, not
    /// [`FilterControl::set`].
    ///
    /// One filter per block at a time. The filter numbers its blocks on from the
    /// block's input ring, so a block whose filter was removed can be given a new one
    /// (a chain rebuilt around it) without restarting the host. It fixes its latency from
    /// the host's ready latency when it is built: a host reporting a new latency needs
    /// the filter removed and added again. The block must be initialised for
    /// [`Self::mixer_rate`] and [`Self::mixer_block_frames`].
    ///
    /// # Errors
    /// As [`Self::add_filter`], and [`AudioError::InvalidConfig`] for a block that isn't
    /// initialised for this mixer or whose host isn't ready.
    pub fn insert_host_filter(
        &self,
        index: usize,
        memory: HostMemory,
    ) -> Result<FilterControl, AudioError> {
        insert_host_filter(&self.plane, ChainTarget::Input(self.id.0), index, memory)
    }

    /// The mixer's sample rate, which a CLAP filter's block is initialised for.
    pub fn mixer_rate(&self) -> u32 {
        self.plane.sample_rate()
    }

    /// Frames per mixer block, which a CLAP filter's block is initialised for.
    pub fn mixer_block_frames(&self) -> usize {
        self.plane.block_frames()
    }

    /// Removes this input. The mixer drops it on its next block and the ring is freed
    /// on the collector thread. False when the input was already removed, or the
    /// command queue could not take the removal (the input stays).
    pub fn remove(&self) -> bool {
        self.plane.remove(self.id.0)
    }

    /// Sets the input's fader, linear, applied before its sends. Clamped to 0..=+24 dB;
    /// NaN is ignored. The mixer ramps to it over one block.
    pub fn set_gain(&self, gain: f32) {
        self.shared.gain.set(gain);
    }

    /// Sets the gain in dB (`-inf` is silence).
    pub fn set_gain_db(&self, db: f32) {
        self.shared.gain.set(db_to_gain(db));
    }

    /// The linear gain.
    pub fn gain(&self) -> f32 {
        self.shared.gain.get()
    }

    /// Mutes or unmutes the input, keeping its gain. A muted input feeds no bus.
    pub fn set_muted(&self, muted: bool) {
        self.shared.gain.set_muted(muted);
    }

    /// Whether the input is muted.
    pub fn muted(&self) -> bool {
        self.shared.gain.muted()
    }

    /// Sets the input's activity gain, linear, clamped to 0..=1; NaN is ignored. It is
    /// applied after the fader and the input's meter, and scales every send: the
    /// engine's audio that follows the program (ADR-0028's `when_shown`). The mixer
    /// ramps to it over one block. A new input starts at 1.
    pub fn set_activity_gain(&self, gain: f32) {
        if !gain.is_nan() {
            self.shared.activity.store(gain.clamp(0.0, 1.0));
        }
    }

    /// The input's activity gain.
    pub fn activity_gain(&self) -> f32 {
        self.shared.activity.load()
    }

    /// Sends the input into `bus` (post-fader) at `gain`, linear, or changes that
    /// send's gain. Clamped to 0..=+24 dB; NaN changes nothing. The mixer ramps to it
    /// over one block, from silence for a new send. A send to a bus that is later
    /// removed does nothing, and so does a send to another mixer's bus.
    pub fn set_send(&self, bus: BusId, gain: f32) {
        if bus.belongs_to(self.plane.tag()) {
            self.shared.sends.set(bus.slot, bus.id, gain);
        }
    }

    /// [`Self::set_send`] in dB (`-inf` keeps the send, silent).
    pub fn set_send_db(&self, bus: BusId, db: f32) {
        self.set_send(bus, db_to_gain(db));
    }

    /// Stops sending the input into `bus`; the mixer ramps the send out over one block.
    /// False when there was no such send, as for another mixer's bus.
    pub fn remove_send(&self, bus: BusId) -> bool {
        bus.belongs_to(self.plane.tag()) && self.shared.sends.clear(bus.slot, bus.id)
    }

    /// The linear gain of the input's send into `bus`, if it has one. `None` for
    /// another mixer's bus.
    pub fn send(&self, bus: BusId) -> Option<f32> {
        if !bus.belongs_to(self.plane.tag()) {
            return None;
        }
        self.shared.sends.get(bus.slot, bus.id)
    }

    /// The input's post-gain levels; restarts the held peak.
    pub fn take_meter(&self) -> MeterReading {
        self.shared.meter.take()
    }

    /// The input's counters and timing estimates.
    pub fn stats(&self) -> InputStats {
        let s = &*self.shared;
        let finite = |v: f64| v.is_finite().then_some(v);
        InputStats {
            packets: s.packets.get(),
            discontinuities: s.discontinuities.get(),
            gap_frames: s.gap_frames.get(),
            resets: s.resets.get(),
            reanchors: s.reanchors.get(),
            outliers: s.outliers.get(),
            timestamps_missing: s.timestamps_missing.get(),
            unanchored_frames: s.unanchored_frames.get(),
            late_frames: s.late_frames.get(),
            overrun_frames: s.overrun_frames.get(),
            underrun_frames: s.underrun_frames.get(),
            drift_ppm: finite(s.drift_ppm.load()),
            jitter_s: finite(s.jitter.load()),
            correction_ppm: s.correction_ppm.load(),
            timing_error_s: finite(s.timing_error.load()),
            failed: s.failed.load(Ordering::Relaxed),
        }
    }
}

/// One input's counters (since it was added) and timing estimates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct InputStats {
    /// Device packets received.
    pub packets: u64,
    /// Packets flagged as discontinuous (not counting the first packet's flag).
    pub discontinuities: u64,
    /// Device frames lost in gaps the clock agreed with, replaced by silence.
    pub gap_frames: u64,
    /// Restarts of the position mapping: backwards or overlong position jumps, gaps the
    /// clock disagreed with, runs of timestamp outliers, and a discontinuity whose stamp
    /// is more than 20 ms off the fitted clock line.
    pub resets: u64,
    /// Times the input was re-placed on the engine time line after its first anchor
    /// (after resets, discontinuities and timing errors beyond 20 ms).
    pub reanchors: u64,
    /// Timestamps rejected as more than 20 ms off the fitted clock line.
    pub outliers: u64,
    /// Packets without a usable timestamp.
    pub timestamps_missing: u64,
    /// Device frames dropped before the first usable timestamp anchored the input.
    pub unanchored_frames: u64,
    /// Engine frames whose audio arrived after the mixer had passed them, or that a
    /// re-anchor skipped.
    pub late_frames: u64,
    /// Engine frames overwritten before the mixer read them (the input ran more than a
    /// ring ahead): dropped oldest.
    pub overrun_frames: u64,
    /// Engine frames the mixer needed but the input had not delivered: mixed as silence.
    pub underrun_frames: u64,
    /// The device's clock drift against the media clock, ppm (positive: fast), once
    /// measurable.
    pub drift_ppm: Option<f64>,
    /// The standard deviation of the device's timestamps around the fitted clock line,
    /// seconds, once measurable.
    pub jitter_s: Option<f64>,
    /// How much faster than nominal the resampler consumes input, ppm. Settles at the
    /// drift.
    pub correction_ppm: f64,
    /// The smoothed timing error, seconds, once measured: the engine time of the next
    /// output frame minus the capture time of the input it holds. Positive means the
    /// audio lands later on the engine time line than it was captured. The controller
    /// holds it near zero.
    pub timing_error_s: Option<f64>,
    /// Whether the device stream reported an error and stopped.
    pub failed: bool,
}

/// Controls and observes one bus: its fader, mute, meter and counters. Cheap to clone;
/// every clone controls the same bus.
#[derive(Clone, Debug)]
pub struct BusControl {
    id: BusId,
    shared: Arc<BusShared>,
    plane: Arc<ControlPlane>,
}

impl BusControl {
    pub(crate) fn new(id: BusId, shared: Arc<BusShared>, plane: Arc<ControlPlane>) -> Self {
        Self { id, shared, plane }
    }

    #[cfg(test)]
    pub(crate) fn shared_for_test(&self) -> Arc<BusShared> {
        Arc::clone(&self.shared)
    }

    /// Appends a filter to the bus's chain, which runs on the sum before its fader.
    /// Allocates the filter here; the mixer installs it at the start of its next block.
    ///
    /// # Errors
    /// As [`InputControl::add_filter`].
    pub fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError> {
        self.insert_filter(usize::MAX, spec)
    }

    /// [`Self::add_filter`] at `index` in the chain (clamped to its end).
    ///
    /// # Errors
    /// As [`InputControl::add_filter`].
    pub fn insert_filter(
        &self,
        index: usize,
        spec: FilterSpec,
    ) -> Result<FilterControl, AudioError> {
        let target = ChainTarget::Bus {
            slot: self.id.slot,
            id: self.id.id,
        };
        insert_filter(&self.plane, target, index, spec)
    }

    /// [`InputControl::insert_host_filter`] for the bus's chain.
    ///
    /// # Errors
    /// As [`InputControl::insert_host_filter`].
    pub fn insert_host_filter(
        &self,
        index: usize,
        memory: HostMemory,
    ) -> Result<FilterControl, AudioError> {
        let target = ChainTarget::Bus {
            slot: self.id.slot,
            id: self.id.id,
        };
        insert_host_filter(&self.plane, target, index, memory)
    }

    /// This bus's id, for [`InputControl::set_send`].
    pub fn id(&self) -> BusId {
        self.id
    }

    /// Removes this bus. The mixer drops it on its next block; sends into it stop
    /// counting. False for the master bus, for a bus already removed, or when the
    /// command queue could not take the removal (the bus stays).
    pub fn remove(&self) -> bool {
        self.plane.remove_bus(self.id.slot, self.id.id)
    }

    /// Sets the bus's fader, linear, applied after the sum. Clamped to 0..=+24 dB; NaN is ignored.
    pub fn set_gain(&self, gain: f32) {
        self.shared.gain.set(gain);
    }

    /// Sets the bus gain in dB.
    pub fn set_gain_db(&self, db: f32) {
        self.shared.gain.set(db_to_gain(db));
    }

    /// The bus's linear gain.
    pub fn gain(&self) -> f32 {
        self.shared.gain.get()
    }

    /// Mutes or unmutes the bus.
    pub fn set_muted(&self, muted: bool) {
        self.shared.gain.set_muted(muted);
    }

    /// The bus output's levels; restarts the held peak.
    pub fn take_meter(&self) -> MeterReading {
        self.shared.meter.take()
    }

    /// The bus's counters.
    pub fn stats(&self) -> BusStats {
        BusStats {
            blocks: self.shared.blocks.get(),
            skipped_frames: self.shared.skipped_frames.get(),
        }
    }
}

/// A bus's counters since it was added (for the master, since the mixer was created).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BusStats {
    /// Blocks mixed.
    pub blocks: u64,
    /// Engine frames skipped because the mixer fell more than half a ring behind the
    /// clock. Counted on the master bus only; every bus skips the same frames.
    pub skipped_frames: u64,
}

/// The master chain (docs/adr/0080-program-master-and-output-chains-v2.md): its filters run
/// on every bus that feeds an output, the master bus and every bus an output tap reads,
/// after that bus's own chain and fader. Monitoring ([`Mixer::bus_block`]) is before it.
/// Each such bus runs its own copy, so filter state never mixes buses. Cheap to clone.
///
/// [`Mixer::bus_block`]: crate::Mixer::bus_block
#[derive(Clone, Debug)]
pub struct MasterControl {
    plane: Arc<ControlPlane>,
}

impl MasterControl {
    pub(crate) fn new(plane: Arc<ControlPlane>) -> Self {
        Self { plane }
    }

    /// Adds a filter at the end of the master chain, one copy per bus. Allocates the
    /// copies on the caller; a bus added later gets its own copy with the filter's
    /// current settings.
    ///
    /// # Errors
    /// The same as [`InputControl::add_filter`].
    pub fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError> {
        self.insert_filter(usize::MAX, spec)
    }

    /// Adds a filter at `index` in the master chain (clamped to the end).
    ///
    /// # Errors
    /// The same as [`InputControl::add_filter`].
    pub fn insert_filter(
        &self,
        index: usize,
        spec: FilterSpec,
    ) -> Result<FilterControl, AudioError> {
        insert_filter(&self.plane, ChainTarget::Master, index, spec)
    }
}

/// One mixer's output tap. Opaque: [`OutputTapControl::id`] produces one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OutputTapId(pub(crate) u64);

/// Controls one output tap: an output's own view of one bus after the master chain,
/// through the tap's own filter chain (ADR-0080's per-output audio chain). An output
/// with a tap encodes its audio from [`Mixer::output_block`] instead of sharing the
/// master bus's encode. Cheap to clone.
///
/// [`Mixer::output_block`]: crate::Mixer::output_block
#[derive(Clone, Debug)]
pub struct OutputTapControl {
    id: OutputTapId,
    bus: BusId,
    plane: Arc<ControlPlane>,
}

impl OutputTapControl {
    pub(crate) fn new(id: OutputTapId, bus: BusId, plane: Arc<ControlPlane>) -> Self {
        Self { id, bus, plane }
    }

    /// This tap's id, for [`Mixer::output_block`](crate::Mixer::output_block).
    pub fn id(&self) -> OutputTapId {
        self.id
    }

    /// The bus it reads.
    pub fn bus(&self) -> BusId {
        self.bus
    }

    /// Adds a filter at the end of the tap's chain. Its latency moves the tap's blocks'
    /// presentation time, not the other outputs'.
    ///
    /// # Errors
    /// The same as [`InputControl::add_filter`].
    pub fn add_filter(&self, spec: FilterSpec) -> Result<FilterControl, AudioError> {
        self.insert_filter(usize::MAX, spec)
    }

    /// Adds a filter at `index` in the tap's chain (clamped to the end).
    ///
    /// # Errors
    /// The same as [`InputControl::add_filter`].
    pub fn insert_filter(
        &self,
        index: usize,
        spec: FilterSpec,
    ) -> Result<FilterControl, AudioError> {
        insert_filter(&self.plane, ChainTarget::OutputTap(self.id.0), index, spec)
    }

    /// [`InputControl::insert_host_filter`] for the tap's chain.
    ///
    /// # Errors
    /// As [`InputControl::insert_host_filter`].
    pub fn insert_host_filter(
        &self,
        index: usize,
        memory: HostMemory,
    ) -> Result<FilterControl, AudioError> {
        insert_host_filter(
            &self.plane,
            ChainTarget::OutputTap(self.id.0),
            index,
            memory,
        )
    }

    /// Removes the tap; its filters go with it. False when it was already removed, or
    /// the command queue is full (the tap stays).
    pub fn remove(&self) -> bool {
        self.plane.remove_tap(self.id.0)
    }
}

#[cfg(test)]
mod tests {
    use super::BusId;
    use crate::{Mixer, MixerConfig};
    use jstream_media::{AudioFormat, ChannelLayout, SampleFormat};

    /// Another mixer's bus is refused (#228 N2); every mixer's master is the same id.
    #[test]
    fn a_bus_of_another_mixer_is_ignored() {
        let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("valid format");
        let mut a = Mixer::new(MixerConfig::default()).expect("mixer");
        let mut b = Mixer::new(MixerConfig::default()).expect("mixer");
        let (_producer, input) = a.add_input(format).expect("input");
        let own = a.add_bus().expect("bus");
        let foreign = b.add_bus().expect("bus");
        // Both are the first extra bus of their mixer: the same slot and number.
        assert_ne!(own.id(), foreign.id());
        input.set_send(foreign.id(), 0.5);
        assert_eq!(input.send(foreign.id()), None);
        assert!(!input.remove_send(foreign.id()));
        assert_eq!(input.send(own.id()), None);
        input.set_send(own.id(), 0.5);
        assert_eq!(input.send(own.id()), Some(0.5));
        assert!(b.bus_block(own.id()).is_none());
        assert!(a.bus_block(own.id()).is_some());
        assert!(b.bus_block(BusId::MASTER).is_some());
        assert!(matches!(
            b.add_output_tap(own.id()),
            Err(crate::AudioError::Removed)
        ));
        assert_eq!(a.bus().id(), BusId::MASTER);
    }
}
