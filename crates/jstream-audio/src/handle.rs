//! Control-plane handles: faders, mutes, sends, filter chains, meters and counters of
//! the inputs and the buses.
//! Every method is a few atomic operations, so the control thread never waits on a
//! real-time thread and vice versa (docs/RULES.md#R-RT-003).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::rt::bus::{BusShared, MASTER_ID};
use crate::rt::chain::{FilterParams, FilterSpec};
use crate::rt::cmd::{ChainTarget, ControlPlane};
use crate::rt::input::InputShared;
use crate::rt::ring::FrameRing;
use crate::rt::shared::db_to_gain;
use crate::{AudioError, MeterReading};

/// One mixer's input. Opaque: only [`InputControl::id`] produces one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputId(pub(crate) u64);

/// One mixer's bus. Opaque: [`BusControl::id`] produces one, and [`BusId::MASTER`]
/// names the master bus. Ids are never reused within a mixer, so the id of a removed
/// bus never names a later one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BusId {
    /// The mixer's buffer slot for the bus.
    pub(crate) slot: usize,
    pub(crate) id: u64,
}

impl BusId {
    /// The master bus: every mixer has it, and every new input sends to it at 0 dB.
    pub const MASTER: Self = Self {
        slot: 0,
        id: MASTER_ID,
    };
}

/// One filter in a mixer's chains. Opaque: [`FilterControl::id`] produces one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FilterId(u64);

/// Edits and removes one filter in an input's or a bus's chain. Cheap to clone.
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

    /// The latency the filter adds to its chain, frames (the limiter's look-ahead; 0 for
    /// the others). Fixed when it was built.
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

    /// Sends the input into `bus` (post-fader) at `gain`, linear, or changes that
    /// send's gain. Clamped to 0..=+24 dB; NaN changes nothing. The mixer ramps to it
    /// over one block, from silence for a new send. A send to a bus that is later
    /// removed does nothing. `bus` must belong to this input's mixer.
    pub fn set_send(&self, bus: BusId, gain: f32) {
        self.shared.sends.set(bus.slot, bus.id, gain);
    }

    /// [`Self::set_send`] in dB (`-inf` keeps the send, silent).
    pub fn set_send_db(&self, bus: BusId, db: f32) {
        self.set_send(bus, db_to_gain(db));
    }

    /// Stops sending the input into `bus`; the mixer ramps the send out over one block.
    /// False when there was no such send.
    pub fn remove_send(&self, bus: BusId) -> bool {
        self.shared.sends.clear(bus.slot, bus.id)
    }

    /// The linear gain of the input's send into `bus`, if it has one.
    pub fn send(&self, bus: BusId) -> Option<f32> {
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
    /// clock disagreed with, and runs of timestamp outliers.
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
