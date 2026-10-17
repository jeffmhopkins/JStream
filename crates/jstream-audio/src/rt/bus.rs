//! Buses and the send matrix (docs/adr/0028-audio-model-v2.md).
//!
//! The mixer holds up to [`MAX_BUSES`] stereo buses in fixed slots. Slot 0 is the
//! master bus, which exists for the mixer's whole life. Every bus has an id the control
//! plane never reuses, so a slot that is freed and given to a new bus is told apart from
//! the old one.
//!
//! Each input has one [`SendParam`] per slot: the bus it targets and its gain. A send
//! counts only while the bus in that slot has the id it targets, so a send to a removed
//! bus does nothing, and does not leak into the bus that later takes its slot. A new
//! input sends to the master at unity (0 dB), as ADR-0028 sets for new sources.
//!
//! Everything here is atomics, so the control plane and the mixer thread never wait on
//! each other (docs/RULES.md#R-RT-003).

use std::sync::atomic::{AtomicU64, Ordering};

use super::shared::{AtomicF32, Counter, GainParam, MAX_GAIN, Meter};

/// Buses a mixer can hold, the master included. Their mix buffers are allocated when
/// the mixer is built, so adding a bus never allocates on the mixer thread.
/// `jstream-core` holds this equal to the collection's limit, `jstream_protocol::MAX_BUSES`.
pub const MAX_BUSES: usize = 16;

/// The master bus's id. It sits in slot 0 and is never removed.
pub(crate) const MASTER_ID: u64 = 0;

/// A send's target when it has none. No bus is ever given this id.
pub(crate) const NO_BUS: u64 = u64::MAX;

/// What the mixer thread and the control plane share for one bus.
#[derive(Debug)]
pub(crate) struct BusShared {
    /// The bus fader and mute, applied after the sum.
    pub(crate) gain: GainParam,
    /// The bus output's levels, post-fader.
    pub(crate) meter: Meter,
    pub(crate) blocks: Counter,
    /// Only the master counts these: the mixer skips blocks for every bus at once.
    pub(crate) skipped_frames: Counter,
}

impl BusShared {
    pub(crate) fn new() -> Self {
        Self {
            gain: GainParam::new(),
            meter: Meter::default(),
            blocks: Counter::default(),
            skipped_frames: Counter::default(),
        }
    }
}

/// One input's send into one bus slot: post-fader, with a linear gain.
#[derive(Debug)]
pub(crate) struct SendParam {
    /// The id of the bus this send feeds, or [`NO_BUS`].
    bus: AtomicU64,
    gain: AtomicF32,
}

impl SendParam {
    const fn off() -> Self {
        Self {
            bus: AtomicU64::new(NO_BUS),
            gain: AtomicF32::new(0.0),
        }
    }

    /// Points the send at `bus` with `gain`, clamped to `0..=MAX_GAIN`; NaN changes
    /// nothing. The gain is stored before the target is published, so once the mixer
    /// sees the new target it also sees the new gain. Until then it may apply the new
    /// gain to the old target for a block. That target is a bus already gone from this
    /// slot, since a slot holds one bus at a time, so no installed bus gets another
    /// send's gain.
    fn set(&self, bus: u64, gain: f32) {
        if gain.is_nan() {
            return;
        }
        self.gain.store(gain.clamp(0.0, MAX_GAIN));
        self.bus.store(bus, Ordering::Release);
    }

    /// Removes the send if it still targets `bus`.
    fn clear(&self, bus: u64) -> bool {
        self.bus
            .compare_exchange(bus, NO_BUS, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    /// The send's gain into `bus`, if it targets it.
    fn get(&self, bus: u64) -> Option<f32> {
        (self.bus.load(Ordering::Acquire) == bus).then(|| self.gain.load())
    }
}

/// An input's row of the send matrix: one send per bus slot.
#[derive(Debug)]
pub(crate) struct Sends([SendParam; MAX_BUSES]);

impl Sends {
    /// A row with only the send to the master, at unity.
    pub(crate) fn new() -> Self {
        let sends = Self([const { SendParam::off() }; MAX_BUSES]);
        sends.set(0, MASTER_ID, 1.0);
        sends
    }

    /// Sets the send in `slot` to feed bus `bus` at `gain` (linear).
    pub(crate) fn set(&self, slot: usize, bus: u64, gain: f32) {
        if let Some(send) = self.0.get(slot) {
            send.set(bus, gain);
        }
    }

    /// Removes the send in `slot` if it feeds `bus`. False when it didn't.
    pub(crate) fn clear(&self, slot: usize, bus: u64) -> bool {
        self.0.get(slot).is_some_and(|send| send.clear(bus))
    }

    /// The gain of the send in `slot` into `bus`, if there is one.
    pub(crate) fn get(&self, slot: usize, bus: u64) -> Option<f32> {
        self.0.get(slot).and_then(|send| send.get(bus))
    }

    /// The gain the mixer applies into bus `bus` in `slot`: the send's, or 0 without one.
    pub(crate) fn target(&self, slot: usize, bus: u64) -> f32 {
        self.get(slot, bus).unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_row_sends_only_to_the_master_at_unity() {
        let s = Sends::new();
        assert_eq!(s.get(0, MASTER_ID), Some(1.0));
        for slot in 1..MAX_BUSES {
            assert_eq!(s.target(slot, 7), 0.0);
        }
    }

    /// A send names its bus by id, so a bus that later takes the same slot gets nothing.
    #[test]
    fn a_send_only_feeds_the_bus_it_names() {
        let s = Sends::new();
        s.set(3, 42, 0.5);
        assert_eq!(s.get(3, 42), Some(0.5));
        assert_eq!(s.target(3, 43), 0.0);
        assert!(!s.clear(3, 43));
        assert!(s.clear(3, 42));
        assert!(!s.clear(3, 42));
        assert_eq!(s.get(3, 42), None);
    }

    #[test]
    fn send_gains_are_clamped_and_nan_is_ignored() {
        let s = Sends::new();
        s.set(1, 5, 1e9);
        assert_eq!(s.get(1, 5), Some(MAX_GAIN));
        s.set(1, 5, -1.0);
        assert_eq!(s.get(1, 5), Some(0.0));
        s.set(1, 5, f32::NAN);
        assert_eq!(s.get(1, 5), Some(0.0));
        s.set(1, 6, f32::NAN);
        assert_eq!(s.get(1, 6), None, "NaN must not retarget the send");
        // Out-of-range slots are ignored rather than panicking.
        s.set(MAX_BUSES, 5, 1.0);
        assert_eq!(s.get(MAX_BUSES, 5), None);
    }
}
