//! The mixer: pulls a fixed-size block from every input's ring on the engine clock,
//! applies each input's fader, sends it into the buses, applies each bus's fader, and
//! meters inputs and buses.
//!
//! **Signal order** (docs/adr/0028-audio-model-v2.md): per input, ring → filter chain
//! → fader and mute → each send's gain → bus sum. Per bus, sum → filter chain → fader
//! and mute → output. Sends are post-fader.
//!
//! **Latency compensation.** After its chain, each input is delayed to the longest
//! input chain's latency and each bus to the longest bus chain's, so the paths line up.
//! A block's `pts` is the media time of the input it was computed from: its first frame
//! minus that total latency ([`Mixer::latency_frames`]). The master bus (slot 0) is the block [`Mixer::mix`] writes out; every
//! bus's last block is also readable with [`Mixer::bus_block`].
//!
//! **Time line.** Engine frame `n` is media time `n / rate` (the media clock's zero,
//! docs/adr/0014-media-clock.md). A block is the frames `[n, n + block)`; its pts is `n`
//! ticks of `1 / rate` seconds. The mixer produces a block once the clock has passed
//! its end by `latency` frames ([`Mixer::poll`]), so every input's audio for that window
//! has had `latency` to arrive (device period, the resampler's look-ahead, and jitter).
//! Inputs that missed it read as silence and are counted as underrun.
//!
//! **Falling behind.** If the mixer thread is stalled so long that the block it is due
//! to mix is more than half a ring behind the clock, it jumps forward to the newest due
//! block and counts the frames it skipped, rather than mixing stale or overwritten data.
//!
//! **Gain** changes ramp linearly across one block, so fader moves don't click. So do
//! send gains, and a send that starts or stops (or whose bus is added) ramps from or to
//! silence.
//!
//! **Meters** are post-fader (each input before its sends) and each bus's output:
//! per channel, the peak held until the control plane takes it, and the latest block's
//! RMS.
//!
//! [`Mixer::mix`] and [`Mixer::poll`] do not allocate, lock, block or log (R-RT-001); the
//! buffers (one per bus slot) are sized in [`Mixer::new`], and an input or bus the
//! control plane adds is installed from a command that was allocated off this thread.

use std::sync::Arc;

use crossbeam_channel::Receiver;
use jstream_media::{MediaTime, Rounding, Timebase};

use super::bus::{BusShared, MASTER_ID, MAX_BUSES, NO_BUS};
use super::chain::Chain;
use super::cmd::{self, ChainTarget, Command, ControlPlane};
use super::input::InputShared;
use super::retire::Retirer;
use super::ring::FrameRing;
use super::shared::levels;
use crate::error::AudioError;
use crate::handle::{BusControl, BusId, InputControl, InputId};

/// How the mixer is set up. Fixed for the mixer's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixerConfig {
    /// The engine sample rate, Hz: every input is resampled to it. Default 48 000.
    pub sample_rate: u32,
    /// Frames per mixed block. Default 1024, one AAC access unit.
    pub block_frames: usize,
    /// How long after a block's end the mixer waits before mixing it, in frames.
    /// Default 1440 (30 ms): one 10 ms device period, the resampler's look-ahead
    /// (1.3 to 1.5 ms), timestamp jitter and thread wake-up, with margin.
    pub latency_frames: usize,
    /// Each input ring's capacity, in frames: a power of two of at least
    /// `2 * (block_frames + latency_frames)`. Default 16384 (341 ms).
    pub ring_frames: usize,
}

impl Default for MixerConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            block_frames: 1024,
            latency_frames: 1440,
            ring_frames: 16_384,
        }
    }
}

impl MixerConfig {
    /// Checks the configuration.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] naming the first field out of range.
    pub fn validate(&self) -> Result<(), AudioError> {
        let bad = |what| Err(AudioError::InvalidConfig { what });
        if !(8_000..=192_000).contains(&self.sample_rate) {
            return bad("sample_rate must be 8000..=192000");
        }
        if !(1..=16_384).contains(&self.block_frames) {
            return bad("block_frames must be 1..=16384");
        }
        if self.latency_frames > self.sample_rate as usize {
            return bad("latency_frames must be at most one second");
        }
        if !self.ring_frames.is_power_of_two()
            || self.ring_frames > 1 << 22
            || self.ring_frames < 2 * (self.block_frames + self.latency_frames)
        {
            return bad(
                "ring_frames must be a power of two, at least 2 * (block_frames + latency_frames), at most 2^22",
            );
        }
        Ok(())
    }
}

/// One mixed block's place on the engine time line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    /// The engine frame of the block's first frame.
    pub frame: i64,
    /// Frames in the block ([`MixerConfig::block_frames`]).
    pub frames: usize,
    /// The block's presentation time on the media clock, in ticks of `1 / sample_rate`
    /// seconds: the media time of the input it was computed from, `frame` minus the
    /// filter chains' latency ([`Mixer::latency_frames`]). Equal to `frame` while no
    /// filter adds latency.
    pub pts: MediaTime,
}

#[derive(Debug)]
struct Slot {
    id: u64,
    ring: Arc<FrameRing>,
    shared: Arc<InputShared>,
    /// The gain applied at the end of the previous block, where the next ramp starts.
    gain_now: f32,
    /// Per bus slot, the send gain applied at the end of the previous block.
    send_now: [f32; MAX_BUSES],
    /// Per bus slot, the bus `send_now` was applied to. When the slot holds another
    /// bus, the send starts again from silence.
    send_bus: [u64; MAX_BUSES],
    chain: Box<Chain>,
}

#[derive(Debug)]
struct BusSlot {
    id: u64,
    shared: Arc<BusShared>,
    /// The fader gain applied at the end of the previous block.
    gain_now: f32,
    chain: Box<Chain>,
}

/// Adds and removes inputs and buses while [`Mixer::poll`] runs on another thread.
///
/// Cloning shares the command queue. [`Mixer::add_input`] and [`Mixer::add_bus`] are
/// the same operations applied before they return, for a mixer that is not running yet.
#[derive(Clone)]
pub struct MixerControl {
    plane: Arc<ControlPlane>,
}

/// The stereo buses, the master among them, and their inputs. Owned by the mixer
/// thread.
#[derive(Debug)]
pub struct Mixer {
    config: MixerConfig,
    timebase: Timebase,
    inputs: Vec<Slot>,
    /// One entry per bus slot; slot 0 is always the master.
    buses: Box<[Option<BusSlot>]>,
    commands: Receiver<Command>,
    plane: Arc<ControlPlane>,
    master: Arc<BusShared>,
    /// The next block's first frame, once started.
    position: Option<i64>,
    scratch: Box<[[f32; 2]]>,
    /// Each bus slot's sum, then its output: `block_frames` frames per slot.
    acc: Box<[[f32; 2]]>,
    /// The longest input chain's latency, frames: every input is delayed to it.
    input_latency: usize,
    /// The longest bus chain's latency, frames: every bus is delayed to it.
    bus_latency: usize,
}

impl Mixer {
    /// A mixer with no inputs and only the master bus.
    ///
    /// # Errors
    /// [`AudioError::InvalidConfig`] when `config` doesn't [validate](MixerConfig::validate),
    /// and [`AudioError::CollectorUnavailable`] when the thread that frees removed
    /// inputs' rings cannot be started.
    pub fn new(config: MixerConfig) -> Result<Self, AudioError> {
        config.validate()?;
        let timebase =
            Timebase::per_second(config.sample_rate).map_err(|_| AudioError::InvalidConfig {
                what: "sample_rate must be non-zero",
            })?;
        let retirer = Retirer::spawn()?;
        let (tx, commands) = crossbeam_channel::bounded(cmd::COMMAND_CAP);
        let plane = Arc::new(ControlPlane::new(
            tx,
            retirer,
            config.sample_rate,
            config.ring_frames,
        ));
        let master = Arc::new(BusShared::new());
        let mut buses: Box<[Option<BusSlot>]> = (0..MAX_BUSES).map(|_| None).collect();
        buses[0] = Some(BusSlot {
            id: MASTER_ID,
            shared: Arc::clone(&master),
            gain_now: 1.0,
            chain: Box::new(Chain::new(config.sample_rate)?),
        });
        Ok(Self {
            config,
            timebase,
            inputs: Vec::with_capacity(cmd::MAX_INPUTS),
            buses,
            commands,
            plane,
            master,
            position: None,
            scratch: vec![[0.0; 2]; config.block_frames].into_boxed_slice(),
            acc: vec![[0.0; 2]; MAX_BUSES * config.block_frames].into_boxed_slice(),
            input_latency: 0,
            bus_latency: 0,
        })
    }

    /// Adds an input of `format` (the device's own rate and layout, interleaved `f32`, as
    /// [`AudioStream::format`](jstream_platform::AudioStream::format) reports it) and
    /// installs it before returning. Allocates: call it from the control thread, or
    /// while setting the mixer up. To add an input while [`Self::poll`] is running on
    /// another thread, use [`MixerControl::add_input`] from [`Self::control`].
    ///
    /// The returned [`InputControl`] also holds the input's ring and counters. Dropping
    /// it early (`let (producer, _) = ...`) is fine: the mixer and the producer hand
    /// their references to the collector thread, so no real-time thread frees them
    /// (R-RT-003).
    ///
    /// # Errors
    /// [`AudioError::UnsupportedFormat`] for planar samples,
    /// [`AudioError::UnsupportedRate`] for a rate more than 4 times above or 6 times below
    /// the engine rate, [`AudioError::TooManyInputs`] past 64 inputs, and
    /// [`AudioError::CommandQueueFull`] when the mixer has not yet applied a full queue
    /// of input changes.
    pub fn add_input(
        &mut self,
        format: jstream_media::AudioFormat,
    ) -> Result<(super::input::InputProducer, InputControl), AudioError> {
        let (producer, control) = self.control().add_input(format)?;
        self.apply_commands();
        Ok((producer, control))
    }

    /// A handle that adds and removes inputs without borrowing the mixer, so it can be
    /// used while [`Self::poll`] runs.
    pub fn control(&self) -> MixerControl {
        MixerControl {
            plane: Arc::clone(&self.plane),
        }
    }

    /// Adds a bus and installs it before returning. A new bus has no sends: point
    /// inputs at it with [`InputControl::set_send`]. Allocates: call it from the
    /// control thread, or while setting the mixer up. While [`Self::poll`] runs on
    /// another thread, use [`MixerControl::add_bus`].
    ///
    /// # Errors
    /// [`AudioError::TooManyBuses`] past 16 buses (the master included), and
    /// [`AudioError::CommandQueueFull`] when the mixer has not yet applied a full queue
    /// of changes.
    pub fn add_bus(&mut self) -> Result<BusControl, AudioError> {
        let bus = self.control().add_bus()?;
        self.apply_commands();
        Ok(bus)
    }

    /// The master bus's control handle. Its output is the block [`Self::mix`] and
    /// [`Self::poll`] write.
    pub fn bus(&self) -> BusControl {
        BusControl::new(
            BusId::MASTER,
            Arc::clone(&self.master),
            Arc::clone(&self.plane),
        )
    }

    /// Bus `bus`'s output for the last block mixed (interleaved stereo, post-fader), or
    /// silence before the first. `None` when the mixer does not hold that bus: it was
    /// removed, or the mixer has not applied its addition yet.
    pub fn bus_block(&self, bus: BusId) -> Option<&[f32]> {
        let slot = self.buses.get(bus.slot)?.as_ref()?;
        if slot.id != bus.id {
            return None;
        }
        let n = self.config.block_frames;
        self.acc
            .get(bus.slot * n..(bus.slot + 1) * n)
            .map(<[[f32; 2]]>::as_flattened)
    }

    /// The filter chains' latency, frames: the longest input chain's plus the longest
    /// bus chain's, as of the last block mixed or command applied. Each block's `pts`
    /// is its first frame minus this, so it changes when a filter with latency is added
    /// or removed.
    pub fn latency_frames(&self) -> usize {
        self.input_latency + self.bus_latency
    }

    /// The configuration.
    pub fn config(&self) -> MixerConfig {
        self.config
    }

    /// The first frame of the next block, once the mixer has started.
    pub fn position(&self) -> Option<i64> {
        self.position
    }

    /// The engine frame at media time `t` (rounded down), or `None` if it doesn't fit.
    pub fn engine_frame(&self, t: MediaTime) -> Option<i64> {
        t.rescale(self.timebase, Rounding::Down)
            .ok()
            .map(MediaTime::ticks)
    }

    /// Mixes the next block if the clock (`now`) has passed its end by the configured
    /// latency, into `out` (interleaved stereo, `2 * block_frames` samples). Returns
    /// `None` when no block is due yet; call it again later. The first call starts the
    /// time line at `now` minus the latency.
    ///
    /// # Errors
    /// [`AudioError::OutputLength`] when `out` has the wrong length.
    pub fn poll(&mut self, now: MediaTime, out: &mut [f32]) -> Result<Option<Block>, AudioError> {
        self.apply_commands();
        self.check_out(out)?;
        let Some(now) = self.engine_frame(now) else {
            return Ok(None);
        };
        let block = self.config.block_frames as i64;
        let latency = self.config.latency_frames as i64;
        let start = now.saturating_sub(latency);
        let pos = *self.position.get_or_insert(start);
        let due = pos.saturating_add(block).saturating_add(latency);
        if now < due {
            return Ok(None);
        }
        let max_lag = (self.config.ring_frames / 2) as i64;
        let pos = if now.saturating_sub(due) > max_lag {
            let newest = start.saturating_sub(block);
            self.master
                .skipped_frames
                .add(newest.saturating_sub(pos).unsigned_abs());
            newest
        } else {
            pos
        };
        self.mix(pos, out).map(Some)
    }

    /// Mixes the block starting at engine frame `frame` into `out` (interleaved stereo,
    /// `2 * block_frames` samples), whatever the clock says, and continues from its end.
    ///
    /// # Errors
    /// [`AudioError::OutputLength`] when `out` has the wrong length.
    pub fn mix(&mut self, frame: i64, out: &mut [f32]) -> Result<Block, AudioError> {
        self.apply_commands();
        self.check_out(out)?;
        let n = self.config.block_frames;
        let end = frame.saturating_add(n as i64);
        let Self {
            inputs,
            buses,
            scratch,
            acc,
            input_latency,
            bus_latency,
            ..
        } = self;
        let (input_latency, bus_latency) = (*input_latency, *bus_latency);
        for (bus, sum) in buses.iter().zip(acc.chunks_exact_mut(n)) {
            if bus.is_some() {
                sum.fill([0.0; 2]);
            }
        }
        for slot in inputs.iter_mut() {
            let rep = slot.ring.read(frame, scratch);
            slot.ring.set_read_pos(end);
            if rep.started {
                slot.shared
                    .underrun_frames
                    .add((n - rep.valid.min(n)) as u64);
            }
            let comp = input_latency - slot.chain.latency().min(input_latency);
            slot.chain.process(scratch, comp);
            let target = slot.shared.gain.effective();
            apply_gain(scratch, slot.gain_now, target);
            slot.gain_now = target;
            let (peak, rms) = levels(scratch);
            slot.shared.meter.publish(peak, rms);
            for (k, (bus, sum)) in buses.iter().zip(acc.chunks_exact_mut(n)).enumerate() {
                let Some(bus) = bus else { continue };
                if slot.send_bus[k] != bus.id {
                    slot.send_bus[k] = bus.id;
                    slot.send_now[k] = 0.0;
                }
                let target = slot.shared.sends.target(k, bus.id);
                add_with_gain(sum, scratch, slot.send_now[k], target);
                slot.send_now[k] = target;
            }
        }
        for (bus, sum) in buses.iter_mut().zip(acc.chunks_exact_mut(n)) {
            let Some(bus) = bus else { continue };
            let comp = bus_latency - bus.chain.latency().min(bus_latency);
            bus.chain.process(sum, comp);
            let target = bus.shared.gain.effective();
            apply_gain(sum, bus.gain_now, target);
            bus.gain_now = target;
            for f in sum.iter_mut() {
                for s in f.iter_mut() {
                    if !s.is_finite() {
                        *s = 0.0;
                    }
                }
            }
            let (peak, rms) = levels(sum);
            bus.shared.meter.publish(peak, rms);
            bus.shared.blocks.add(1);
        }
        // Slot 0 is the master.
        out.copy_from_slice(acc[..n].as_flattened());
        self.position = Some(end);
        let latency = (input_latency + bus_latency) as i64;
        Ok(Block {
            frame,
            frames: n,
            pts: MediaTime::new(frame.saturating_sub(latency), self.timebase),
        })
    }

    /// Installs inputs, buses and filters the control plane queued, retires the ones it
    /// removed, and applies filter edits. Pushing and inserting stay within the capacity
    /// reserved in [`Mixer::new`] and when each chain was built.
    fn apply_commands(&mut self) {
        let mut changed = false;
        while let Ok(command) = self.commands.try_recv() {
            changed = true;
            match command {
                Command::Add {
                    id,
                    ring,
                    shared,
                    chain,
                } => {
                    if self.inputs.len() >= cmd::MAX_INPUTS {
                        self.plane.retirer().ring(ring);
                        self.plane.retirer().shared(shared);
                        self.plane.retirer().chain(chain);
                        continue;
                    }
                    if let Some(pos) = self.position {
                        ring.set_read_pos(pos);
                    }
                    let gain_now = shared.gain.effective();
                    let send_bus: [u64; MAX_BUSES] = std::array::from_fn(|k| {
                        self.buses[k].as_ref().map_or(NO_BUS, |bus| bus.id)
                    });
                    let send_now = std::array::from_fn(|k| shared.sends.target(k, send_bus[k]));
                    self.inputs.push(Slot {
                        id,
                        ring,
                        shared,
                        gain_now,
                        send_now,
                        send_bus,
                        chain,
                    });
                }
                Command::Remove(id) => self.retire_id(id),
                Command::AddBus {
                    slot,
                    id,
                    shared,
                    chain,
                } => match self.buses.get_mut(slot) {
                    Some(entry @ None) => {
                        let gain_now = shared.gain.effective();
                        *entry = Some(BusSlot {
                            id,
                            shared,
                            gain_now,
                            chain,
                        });
                    }
                    // The control plane only hands out free slots; refuse anything else.
                    _ => {
                        self.plane.retirer().bus(shared);
                        self.plane.retirer().chain(chain);
                    }
                },
                Command::RemoveBus { slot, id } => self.retire_bus(slot, id),
                Command::AddFilter {
                    target,
                    index,
                    id,
                    kernel,
                } => {
                    let refused = match self.chain_mut(target) {
                        Some(chain) => chain.insert(index, id, kernel).err(),
                        None => Some(kernel),
                    };
                    if let Some(kernel) = refused {
                        self.plane.retirer().kernel(kernel);
                    }
                }
                Command::RemoveFilter { target, id } => {
                    if let Some(kernel) = self.chain_mut(target).and_then(|c| c.remove(id)) {
                        self.plane.retirer().kernel(kernel);
                    }
                }
                Command::EditFilter { target, id, params } => {
                    if let Some(chain) = self.chain_mut(target) {
                        chain.edit(id, &params);
                    }
                }
            }
        }
        if changed {
            self.input_latency = self
                .inputs
                .iter()
                .map(|slot| slot.chain.latency())
                .max()
                .unwrap_or(0);
            self.bus_latency = self
                .buses
                .iter()
                .flatten()
                .map(|bus| bus.chain.latency())
                .max()
                .unwrap_or(0);
        }
    }

    /// The chain a filter command names, if its input or bus is installed.
    fn chain_mut(&mut self, target: ChainTarget) -> Option<&mut Chain> {
        match target {
            ChainTarget::Input(id) => self
                .inputs
                .iter_mut()
                .find(|slot| slot.id == id)
                .map(|slot| &mut *slot.chain),
            ChainTarget::Bus { slot, id } => self
                .buses
                .get_mut(slot)?
                .as_mut()
                .filter(|bus| bus.id == id)
                .map(|bus| &mut *bus.chain),
        }
    }

    fn retire_bus(&mut self, slot: usize, id: u64) {
        let Some(entry) = self.buses.get_mut(slot) else {
            return;
        };
        if id == MASTER_ID || entry.as_ref().is_none_or(|bus| bus.id != id) {
            return;
        }
        if let Some(bus) = entry.take() {
            self.plane.retirer().bus(bus.shared);
            self.plane.retirer().chain(bus.chain);
        }
    }

    fn retire_id(&mut self, id: u64) {
        let Some(index) = self.inputs.iter().position(|slot| slot.id == id) else {
            return;
        };
        let slot = self.inputs.swap_remove(index);
        self.plane.retirer().ring(slot.ring);
        self.plane.retirer().shared(slot.shared);
        self.plane.retirer().chain(slot.chain);
    }

    fn check_out(&self, out: &[f32]) -> Result<(), AudioError> {
        let expected = self.config.block_frames * 2;
        if out.len() == expected {
            Ok(())
        } else {
            Err(AudioError::OutputLength {
                expected,
                got: out.len(),
            })
        }
    }
}

impl Drop for Mixer {
    /// Retired rings are freed on the collector thread, not on whichever thread drops
    /// the mixer. Pending adds that the mixer never installed go there too.
    fn drop(&mut self) {
        loop {
            match self.commands.try_recv() {
                Ok(Command::Add {
                    ring,
                    shared,
                    chain,
                    ..
                }) => {
                    self.plane.retirer().ring(ring);
                    self.plane.retirer().shared(shared);
                    self.plane.retirer().chain(chain);
                }
                Ok(Command::Remove(id)) => self.retire_id(id),
                Ok(Command::AddBus { shared, chain, .. }) => {
                    self.plane.retirer().bus(shared);
                    self.plane.retirer().chain(chain);
                }
                Ok(Command::RemoveBus { slot, id }) => self.retire_bus(slot, id),
                Ok(Command::AddFilter { kernel, .. }) => self.plane.retirer().kernel(kernel),
                Ok(Command::RemoveFilter { .. } | Command::EditFilter { .. }) => {}
                Err(_) => break,
            }
        }
        for slot in self.inputs.drain(..) {
            self.plane.retirer().ring(slot.ring);
            self.plane.retirer().shared(slot.shared);
            self.plane.retirer().chain(slot.chain);
        }
        for bus in self.buses.iter_mut().filter_map(Option::take) {
            self.plane.retirer().bus(bus.shared);
            self.plane.retirer().chain(bus.chain);
        }
    }
}

impl MixerControl {
    /// Adds an input while the mixer is running. The mixer installs it at the start of
    /// its next [`Mixer::poll`] or [`Mixer::mix`]. The returned producer may push audio
    /// immediately; the ring holds it until then.
    ///
    /// # Errors
    /// The same as [`Mixer::add_input`].
    pub fn add_input(
        &self,
        format: jstream_media::AudioFormat,
    ) -> Result<(super::input::InputProducer, InputControl), AudioError> {
        let (producer, id, shared, ring) = self.plane.add(format)?;
        Ok((
            producer,
            InputControl::new(InputId(id), shared, ring, Arc::clone(&self.plane)),
        ))
    }

    /// Adds a bus while the mixer is running. The mixer installs it at the start of its
    /// next [`Mixer::poll`] or [`Mixer::mix`].
    ///
    /// # Errors
    /// The same as [`Mixer::add_bus`].
    pub fn add_bus(&self) -> Result<BusControl, AudioError> {
        let (slot, id, shared) = self.plane.add_bus()?;
        Ok(BusControl::new(
            BusId { slot, id },
            shared,
            Arc::clone(&self.plane),
        ))
    }
}

/// Scales `frames` by a gain ramping linearly from `from` (before the first frame) to
/// `to` (at the last).
fn apply_gain(frames: &mut [[f32; 2]], from: f32, to: f32) {
    if from == to {
        if to != 1.0 {
            for f in frames.iter_mut() {
                f[0] *= to;
                f[1] *= to;
            }
        }
        return;
    }
    let n = frames.len() as f32;
    for (i, f) in frames.iter_mut().enumerate() {
        let g = from + (to - from) * ((i + 1) as f32 / n);
        f[0] *= g;
        f[1] *= g;
    }
}

/// Adds `src` scaled by a gain ramping linearly from `from` to `to` into `sum`, as
/// [`apply_gain`] ramps. A send that is silent before and after costs nothing.
fn add_with_gain(sum: &mut [[f32; 2]], src: &[[f32; 2]], from: f32, to: f32) {
    if from == to {
        if to == 0.0 {
            return;
        }
        for (a, s) in sum.iter_mut().zip(src) {
            a[0] += s[0] * to;
            a[1] += s[1] * to;
        }
        return;
    }
    let n = src.len() as f32;
    for (i, (a, s)) in sum.iter_mut().zip(src).enumerate() {
        let g = from + (to - from) * ((i + 1) as f32 / n);
        a[0] += s[0] * g;
        a[1] += s[1] * g;
    }
}

#[cfg(test)]
mod tests {
    use jstream_media::{AudioFormat, ChannelLayout, SampleFormat};
    use jstream_platform::{AudioPacket, AudioPacketFlags};

    use super::*;
    use crate::InputProducer;

    const RATE: u32 = 48_000;

    fn stereo48() -> AudioFormat {
        AudioFormat::new(RATE, ChannelLayout::Stereo, SampleFormat::F32Interleaved).unwrap()
    }

    fn config(block: usize) -> MixerConfig {
        MixerConfig {
            block_frames: block,
            latency_frames: 480,
            ring_frames: 8192,
            ..MixerConfig::default()
        }
    }

    /// Feeds `secs` of constant `[l, r]` frames in 10 ms packets stamped exactly, with
    /// the first frame at clock time 0.
    fn feed(p: &mut InputProducer, l: f32, r: f32, secs: f64) {
        let frames = 480;
        let samples: Vec<f32> = (0..frames).flat_map(|_| [l, r]).collect();
        let packets = (secs * 100.0) as u64;
        for i in 0..packets {
            p.push_packet(&AudioPacket {
                samples: &samples,
                device_position: i * frames as u64,
                time: Some(MediaTime::new(
                    (i * frames as u64) as i64,
                    Timebase::per_second(RATE).unwrap(),
                )),
                flags: AudioPacketFlags::default(),
            });
        }
    }

    #[test]
    fn config_is_validated() {
        assert!(MixerConfig::default().validate().is_ok());
        let bad = [
            MixerConfig {
                sample_rate: 0,
                ..MixerConfig::default()
            },
            MixerConfig {
                block_frames: 0,
                ..MixerConfig::default()
            },
            MixerConfig {
                ring_frames: 10_000,
                ..MixerConfig::default()
            },
            MixerConfig {
                ring_frames: 4096,
                ..MixerConfig::default()
            },
            MixerConfig {
                latency_frames: 96_000,
                ..MixerConfig::default()
            },
        ];
        for c in bad {
            assert!(
                matches!(Mixer::new(c), Err(AudioError::InvalidConfig { .. })),
                "{c:?}"
            );
        }
    }

    #[test]
    fn unsupported_inputs_are_rejected() {
        let mut m = Mixer::new(MixerConfig::default()).unwrap();
        let planar =
            AudioFormat::new(RATE, ChannelLayout::Stereo, SampleFormat::F32Planar).unwrap();
        assert!(matches!(
            m.add_input(planar),
            Err(AudioError::UnsupportedFormat)
        ));
        for rate in [7_999, 192_001] {
            let f =
                AudioFormat::new(rate, ChannelLayout::Mono, SampleFormat::F32Interleaved).unwrap();
            assert!(
                matches!(m.add_input(f), Err(AudioError::UnsupportedRate { .. })),
                "{rate}"
            );
        }
        for rate in [8_000, 44_100, 192_000] {
            let f =
                AudioFormat::new(rate, ChannelLayout::Mono, SampleFormat::F32Interleaved).unwrap();
            assert!(m.add_input(f).is_ok(), "{rate}");
        }
    }

    /// Whichever of the producer (audio thread) and the mixer (mixer thread) is dropped
    /// last, the input's ring and counters outlive both while a control handle is
    /// held (R-RT-003), and are freed once it is gone too.
    #[test]
    fn control_handle_holds_the_last_shared_reference() {
        for mixer_first in [true, false] {
            let mut m = Mixer::new(config(480)).unwrap();
            let (p, control) = m.add_input(stereo48()).unwrap();
            let ring = Arc::downgrade(&m.inputs[0].ring);
            let shared = Arc::downgrade(&m.inputs[0].shared);
            if mixer_first {
                drop(m);
                drop(p);
            } else {
                drop(p);
                drop(m);
            }
            assert!(ring.upgrade().is_some(), "ring freed by an RT handle");
            assert!(shared.upgrade().is_some(), "counters freed by an RT handle");
            drop(control);
            // The collector frees the mixer's and producer's references on its own
            // schedule, so the last free happens on whichever side goes last.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while ring.upgrade().is_some() || shared.upgrade().is_some() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "ring or counters leaked"
                );
                std::thread::yield_now();
            }
        }
    }

    /// Two inputs with gains sum on the bus; meters see post-gain levels.
    #[test]
    fn gains_mix_and_meter() {
        let mut m = Mixer::new(config(480)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        let (mut b, cb) = m.add_input(stereo48()).unwrap();
        feed(&mut a, 0.5, -0.5, 1.0);
        feed(&mut b, 0.25, 0.25, 1.0);
        ca.set_gain(0.5);
        cb.set_gain_db(6.0);
        let mut out = vec![0.0; 960];
        // The 8192-frame rings hold frames 39744..47936 of the 47936 written.
        // The first block ramps from unity to the new gains; mix it and discard it.
        m.mix(40_000, &mut out).unwrap();
        ca.take_meter();
        m.bus().take_meter();
        let blk = m.mix(40_480, &mut out).unwrap();
        assert_eq!(blk.frame, 40_480);
        assert_eq!(
            blk.pts,
            MediaTime::new(40_480, Timebase::per_second(RATE).unwrap())
        );
        let g = 10f32.powf(6.0 / 20.0);
        let want = [0.25 + 0.25 * g, -0.25 + 0.25 * g];
        for f in out.chunks_exact(2) {
            assert!(
                (f[0] - want[0]).abs() < 1e-5 && (f[1] - want[1]).abs() < 1e-5,
                "{f:?}"
            );
        }
        let ma = ca.take_meter();
        assert!((ma.peak[0] - 0.25).abs() < 1e-5 && (ma.rms[1] - 0.25).abs() < 1e-5);
        let bus = m.bus().take_meter();
        assert!((bus.peak[0] - want[0]).abs() < 1e-5);
        assert!((bus.rms[1] - want[1].abs()).abs() < 1e-5);
        // Mute and bus gain.
        ca.set_muted(true);
        m.bus().set_gain(2.0);
        m.mix(40_960, &mut out).unwrap(); // ramp
        m.mix(41_440, &mut out).unwrap();
        assert!((out[0] - 2.0 * 0.25 * g).abs() < 1e-5, "{}", out[0]);
        assert_eq!(ca.stats().underrun_frames, 0);
        assert_eq!(m.bus().stats().blocks, 4);
    }

    fn assert_frames(block: &[f32], want: [f32; 2], what: &str) {
        for f in block.chunks_exact(2) {
            assert!(
                (f[0] - want[0]).abs() < 1e-5 && (f[1] - want[1]).abs() < 1e-5,
                "{what}: {f:?}, want {want:?}"
            );
        }
    }

    /// Sends are post-fader and each bus sums only what is sent to it, with its own
    /// fader and mute, and meters its own output.
    #[test]
    fn sends_route_inputs_into_buses_after_the_fader() {
        let mut m = Mixer::new(config(480)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        let (mut b, cb) = m.add_input(stereo48()).unwrap();
        let aux = m.add_bus().unwrap();
        assert_ne!(aux.id(), BusId::MASTER);
        assert_eq!(
            ca.send(BusId::MASTER),
            Some(1.0),
            "new inputs feed the master"
        );
        assert_eq!(ca.send(aux.id()), None, "a new bus has no sends");
        feed(&mut a, 0.5, -0.5, 1.0);
        feed(&mut b, 0.25, 0.25, 1.0);
        ca.set_gain(2.0);
        ca.set_send(aux.id(), 0.5);
        cb.set_send_db(aux.id(), 0.0);
        assert!(cb.remove_send(BusId::MASTER));
        assert!(!cb.remove_send(BusId::MASTER));
        let mut out = vec![0.0; 960];
        m.mix(40_000, &mut out).unwrap(); // ramps
        m.bus().take_meter();
        aux.take_meter();
        m.mix(40_480, &mut out).unwrap();
        assert_frames(&out, [1.0, -1.0], "master");
        assert_eq!(m.bus_block(BusId::MASTER).unwrap(), &out[..]);
        let want = [0.5 + 0.25, -0.5 + 0.25];
        assert_frames(m.bus_block(aux.id()).unwrap(), want, "aux");
        let meter = aux.take_meter();
        assert!((meter.peak[1] - 0.25).abs() < 1e-5 && (meter.rms[0] - 0.75).abs() < 1e-5);
        // The input meter is post-fader, before the sends.
        assert!((ca.take_meter().peak[0] - 1.0).abs() < 1e-5);

        // A muted source feeds no bus; a bus fader touches only its own output.
        ca.set_muted(true);
        aux.set_gain(2.0);
        m.mix(40_960, &mut out).unwrap();
        m.mix(41_440, &mut out).unwrap();
        assert_frames(&out, [0.0, 0.0], "master with its only input muted");
        assert_frames(m.bus_block(aux.id()).unwrap(), [0.5, 0.5], "aux");
        aux.set_muted(true);
        m.mix(41_920, &mut out).unwrap();
        m.mix(42_400, &mut out).unwrap();
        assert_frames(m.bus_block(aux.id()).unwrap(), [0.0, 0.0], "muted aux");
        assert_eq!(aux.stats().blocks, 6);
        assert_eq!(m.bus().stats().blocks, 6);
    }

    /// A send that starts ramps up from silence over one block, and one that is removed
    /// ramps down to it, so routing changes don't click.
    #[test]
    fn sends_ramp_in_and_out() {
        let mut m = Mixer::new(config(4)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        let aux = m.add_bus().unwrap();
        feed(&mut a, 1.0, 1.0, 1.0);
        let mut out = vec![0.0; 8];
        m.mix(40_000, &mut out).unwrap();
        assert_frames(m.bus_block(aux.id()).unwrap(), [0.0, 0.0], "no send yet");
        ca.set_send(aux.id(), 1.0);
        m.mix(40_004, &mut out).unwrap();
        let ramp: Vec<f32> = m
            .bus_block(aux.id())
            .unwrap()
            .iter()
            .step_by(2)
            .copied()
            .collect();
        assert_eq!(ramp, [0.25, 0.5, 0.75, 1.0]);
        m.mix(40_008, &mut out).unwrap();
        assert_frames(m.bus_block(aux.id()).unwrap(), [1.0, 1.0], "steady");
        assert!(ca.remove_send(aux.id()));
        m.mix(40_012, &mut out).unwrap();
        let ramp: Vec<f32> = m
            .bus_block(aux.id())
            .unwrap()
            .iter()
            .step_by(2)
            .copied()
            .collect();
        assert_eq!(ramp, [0.75, 0.5, 0.25, 0.0]);
        // The master was untouched throughout.
        assert_frames(&out, [1.0, 1.0], "master");
    }

    /// Removing a bus drops it from the mix; its slot can go to a new bus, which the
    /// old bus's sends do not reach. The master can't be removed, and the mixer holds
    /// at most 16 buses.
    #[test]
    fn buses_are_added_and_removed_while_the_mixer_runs() {
        let mut m = Mixer::new(config(480)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        feed(&mut a, 0.5, 0.5, 1.0);
        let ops = m.control();
        let aux = std::thread::spawn(move || ops.add_bus().unwrap())
            .join()
            .unwrap();
        assert_eq!(
            m.bus_block(aux.id()),
            None,
            "not installed before the next block"
        );
        ca.set_send(aux.id(), 1.0);
        let mut out = vec![0.0; 960];
        m.mix(40_000, &mut out).unwrap();
        m.mix(40_480, &mut out).unwrap();
        assert_frames(m.bus_block(aux.id()).unwrap(), [0.5, 0.5], "aux");
        let shared = Arc::downgrade(&aux.shared_for_test());

        assert!(!m.bus().remove(), "the master stays");
        assert!(aux.remove());
        assert!(!aux.remove());
        m.mix(40_960, &mut out).unwrap();
        assert_eq!(m.bus_block(aux.id()), None);
        assert_frames(&out, [0.5, 0.5], "master");

        let next = m.add_bus().unwrap();
        assert_eq!(next.id().slot, aux.id().slot, "the freed slot is reused");
        assert_ne!(next.id(), aux.id());
        m.mix(41_440, &mut out).unwrap();
        assert_frames(
            m.bus_block(next.id()).unwrap(),
            [0.0, 0.0],
            "old send leaked",
        );
        assert_eq!(ca.send(next.id()), None);

        // The removed bus's state is freed on the collector once its handle is gone.
        drop(aux);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while shared.upgrade().is_some() {
            assert!(std::time::Instant::now() < deadline, "bus state leaked");
            std::thread::yield_now();
        }

        let mut extra = Vec::new();
        while extra.len() < 14 {
            extra.push(m.add_bus().unwrap());
        }
        assert!(matches!(m.add_bus(), Err(AudioError::TooManyBuses)));
        assert!(extra[0].remove());
        assert!(m.add_bus().is_ok());
    }

    /// A new input's sends start at their gains, like its fader: its first block
    /// reaches the master at full level rather than ramping in from silence.
    #[test]
    fn a_new_input_feeds_the_master_from_its_first_block() {
        let mut m = Mixer::new(config(4)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        feed(&mut a, 1.0, 1.0, 1.0);
        let mut out = vec![0.0; 8];
        m.mix(40_000, &mut out).unwrap();
        assert_frames(&out, [1.0, 1.0], "first block at full level");
        assert_eq!(ca.stats().underrun_frames, 0);
    }

    /// Feeds `secs` of `f(frame)` stereo frames in 10 ms packets stamped exactly.
    fn feed_with(p: &mut InputProducer, f: impl Fn(u64) -> [f32; 2], secs: f64) {
        let frames = 480u64;
        let packets = (secs * 100.0) as u64;
        for i in 0..packets {
            let samples: Vec<f32> = (0..frames).flat_map(|n| f(i * frames + n)).collect();
            p.push_packet(&AudioPacket {
                samples: &samples,
                device_position: i * frames,
                time: Some(MediaTime::new(
                    (i * frames) as i64,
                    Timebase::per_second(RATE).unwrap(),
                )),
                flags: AudioPacketFlags::default(),
            });
        }
    }

    /// An input's chain runs before its fader and a bus's chain on its sum, and edits
    /// and removals reach the mixer through the command queue.
    #[test]
    fn filter_chains_run_on_inputs_and_buses_and_are_edited_live() {
        use crate::{FilterParams, FilterSpec};
        let mut m = Mixer::new(config(480)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        feed(&mut a, 0.5, 0.5, 1.0);
        let input_gain = ca.add_filter(FilterSpec::Gain { gain_db: -6.0 }).unwrap();
        ca.set_gain(2.0);
        let bus_gain = m
            .bus()
            .add_filter(FilterSpec::Gain { gain_db: 6.0 })
            .unwrap();
        assert_eq!(input_gain.latency_frames(), 0);
        let mut out = vec![0.0; 960];
        // The gain kernels ramp over 10 ms (480 frames): settle, then measure.
        m.mix(40_000, &mut out).unwrap();
        m.mix(40_480, &mut out).unwrap();
        let g6 = 10f32.powf(6.0 / 20.0);
        let want = 0.5 / g6 * 2.0 * g6;
        assert_frames(&out, [want, want], "input chain, fader, bus chain");
        // The input meter is after the chain and the fader.
        assert!((ca.take_meter().peak[0] - 0.5 / g6 * 2.0).abs() < 1e-5);

        input_gain.set(FilterParams::Gain { gain_db: 0.0 }).unwrap();
        m.mix(40_960, &mut out).unwrap();
        m.mix(41_440, &mut out).unwrap();
        assert_frames(&out, [g6, g6], "after the edit");

        assert!(bus_gain.remove());
        assert!(!bus_gain.remove());
        m.mix(41_920, &mut out).unwrap();
        assert_frames(&out, [1.0, 1.0], "bus filter removed");
        assert!(matches!(
            bus_gain.set(FilterParams::Gain { gain_db: 0.0 }),
            Err(AudioError::Removed)
        ));
        assert_eq!(m.latency_frames(), 0);
    }

    /// A chain with latency (a limiter's look-ahead) delays the other inputs and buses
    /// to match, and the block's pts moves back by the total, so every path lines up and
    /// the stamp is the media time of the input in the block.
    #[test]
    fn latency_is_compensated_and_stamped() {
        use crate::FilterSpec;
        use crate::filter::LimiterParams;
        let tone =
            |n: u64| (0.25 * (std::f64::consts::TAU * 100.0 * n as f64 / 48_000.0).sin()) as f32;
        let mut m = Mixer::new(config(480)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        let (mut b, _cb) = m.add_input(stereo48()).unwrap();
        // The same tone, on the left from A and on the right from B.
        feed_with(&mut a, |n| [tone(n), 0.0], 1.0);
        feed_with(&mut b, |n| [0.0, tone(n)], 1.0);
        let limiter = |lookahead_ms| FilterSpec::Limiter {
            lookahead_ms,
            params: LimiterParams::default(),
        };
        let lim = ca.add_filter(limiter(5.0)).unwrap();
        assert_eq!(lim.latency_frames(), 240);
        let aux = m.add_bus().unwrap();
        ca.set_send(aux.id(), 1.0);
        let aux_lim = aux.add_filter(limiter(2.0)).unwrap();
        assert_eq!(aux_lim.latency_frames(), 96);

        let mut out = vec![0.0; 960];
        let blk = m.mix(40_000, &mut out).unwrap();
        assert_eq!(m.latency_frames(), 240 + 96);
        // The send into the aux bus ramps in over this block; the aux chain holds its
        // tail into the next one, so compare from the third block.
        m.mix(40_480, &mut out).unwrap();
        assert_eq!(
            blk.pts,
            MediaTime::new(40_000 - 240 - 96, Timebase::per_second(RATE).unwrap())
        );
        for frame in [40_960, 41_440] {
            m.mix(frame, &mut out).unwrap();
            // A (with the limiter) and B (compensated) line up on the master...
            for (i, f) in out.chunks_exact(2).enumerate() {
                assert!((f[0] - f[1]).abs() < 1e-3, "frame {i}: {f:?}");
            }
            // ...which is delayed by both chains: the input's and the aux bus's.
            for (i, f) in out.chunks_exact(2).enumerate() {
                let want = tone((frame + i as i64 - 240 - 96) as u64);
                assert!((f[0] - want).abs() < 2e-3, "frame {i}: {} vs {want}", f[0]);
            }
            // The aux bus, which has the longest bus chain, is delayed by its own.
            let aux_out = m.bus_block(aux.id()).unwrap();
            for (i, f) in aux_out.chunks_exact(2).enumerate() {
                let want = tone((frame + i as i64 - 240 - 96) as u64);
                assert!(
                    (f[0] - want).abs() < 2e-3,
                    "aux frame {i}: {} vs {want}",
                    f[0]
                );
            }
        }
        // Removing the latency brings the stamp back to the mixed frame.
        assert!(lim.remove() && aux_lim.remove());
        let blk = m.mix(41_920, &mut out).unwrap();
        assert_eq!(m.latency_frames(), 0);
        assert_eq!(blk.pts.ticks(), 41_920);
    }

    #[test]
    fn filter_additions_and_edits_are_checked() {
        use crate::filter::{GateParams, LimiterParams};
        use crate::{FilterParams, FilterSpec};
        let mut m = Mixer::new(config(480)).unwrap();
        let (_a, ca) = m.add_input(stereo48()).unwrap();
        // At most 100 ms of latency per chain: five 20 ms limiters, not six.
        let limiter = FilterSpec::Limiter {
            lookahead_ms: 20.0,
            params: LimiterParams::default(),
        };
        let mut chain = Vec::new();
        for _ in 0..5 {
            chain.push(ca.add_filter(limiter).unwrap());
        }
        assert!(matches!(
            ca.add_filter(limiter),
            Err(AudioError::InvalidConfig { .. })
        ));
        // At most 8 filters.
        for _ in 0..3 {
            chain.push(ca.add_filter(FilterSpec::Gain { gain_db: 0.0 }).unwrap());
        }
        assert!(matches!(
            ca.add_filter(FilterSpec::Gain { gain_db: 0.0 }),
            Err(AudioError::TooManyFilters)
        ));
        // Out of range, or another kind's parameters.
        assert!(matches!(
            ca.add_filter(FilterSpec::Gain { gain_db: 99.0 }),
            Err(AudioError::TooManyFilters | AudioError::InvalidConfig { .. })
        ));
        let gain = &chain[5];
        assert!(matches!(
            gain.set(FilterParams::NoiseGate(GateParams::default())),
            Err(AudioError::InvalidConfig { .. })
        ));
        assert!(matches!(
            gain.set(FilterParams::Gain { gain_db: 99.0 }),
            Err(AudioError::InvalidConfig { .. })
        ));
        assert!(gain.set(FilterParams::Gain { gain_db: -3.0 }).is_ok());
        let aux = m.add_bus().unwrap();
        let delay = aux
            .add_filter(FilterSpec::Delay {
                max_ms: 10.0,
                delay_ms: 5.0,
            })
            .unwrap();
        assert_eq!(delay.latency_frames(), 0, "a delay is a shift, not latency");
        assert!(delay.set(FilterParams::Delay { delay_ms: 10.0 }).is_ok());
        assert!(matches!(
            delay.set(FilterParams::Delay { delay_ms: 11.0 }),
            Err(AudioError::InvalidConfig { .. })
        ));
        // Once the input is gone, its filters are too.
        assert!(ca.remove());
        assert!(matches!(
            ca.add_filter(FilterSpec::Gain { gain_db: 0.0 }),
            Err(AudioError::Removed)
        ));
        assert!(!chain[0].remove());
        let mut out = vec![0.0; 960];
        m.mix(0, &mut out).unwrap();
        assert_eq!(m.latency_frames(), 0);
    }

    #[test]
    fn gain_ramps_across_one_block() {
        let mut frames = [[1.0f32; 2]; 4];
        apply_gain(&mut frames, 0.0, 1.0);
        assert_eq!(frames.map(|f| f[0]), [0.25, 0.5, 0.75, 1.0]);
        let mut frames = [[1.0f32; 2]; 2];
        apply_gain(&mut frames, 0.5, 0.5);
        assert_eq!(frames, [[0.5; 2]; 2]);
        let mut sum = [[1.0f32; 2]; 4];
        add_with_gain(&mut sum, &[[1.0; 2]; 4], 1.0, 0.0);
        assert_eq!(sum.map(|f| f[1]), [1.75, 1.5, 1.25, 1.0]);
        add_with_gain(&mut sum, &[[1.0; 2]; 4], 0.0, 0.0);
        assert_eq!(sum.map(|f| f[1]), [1.75, 1.5, 1.25, 1.0]);
    }

    /// Reading where the input has no data is silence, counted as underrun once the
    /// input has started; before it starts nothing is counted.
    #[test]
    fn underrun_is_silence_and_counted() {
        let mut m = Mixer::new(config(480)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        let mut out = vec![1.0; 960];
        m.mix(0, &mut out).unwrap();
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(ca.stats().underrun_frames, 0);
        feed(&mut a, 0.5, 0.5, 0.1); // frames 0..4800, minus the look-ahead
        m.mix(4_480, &mut out).unwrap();
        let st = ca.stats();
        // 4800 - 64 frames are written; 4480..4736 valid, the rest underrun.
        assert_eq!(st.underrun_frames, 480 - (4_736 - 4_480));
        assert!(out[..2 * 256].iter().all(|&s| (s - 0.5).abs() < 1e-4));
        assert!(out[2 * 256..].iter().all(|&s| s == 0.0));
    }

    /// An input far ahead of the mixer overwrites unread frames: dropped oldest, counted.
    #[test]
    fn overrun_drops_the_oldest_and_counts() {
        let mut m = Mixer::new(config(480)).unwrap();
        let (mut a, ca) = m.add_input(stereo48()).unwrap();
        let mut out = vec![0.0; 960];
        m.mix(0, &mut out).unwrap(); // read position 480
        feed(&mut a, 0.5, 0.5, 1.0); // 48000 frames into an 8192-frame ring
        let st = ca.stats();
        let written = 48_000 - 64;
        assert_eq!(st.overrun_frames, written - 8192 - 480);
        assert_eq!(st.late_frames, 480);
    }

    #[test]
    fn poll_produces_blocks_on_the_clock() {
        let mut m = Mixer::new(config(1024)).unwrap();
        let tb = Timebase::per_second(RATE).unwrap();
        let mut out = vec![0.0; 2048];
        // Starts at now - latency; nothing is due until a block has elapsed.
        assert_eq!(m.poll(MediaTime::new(10_000, tb), &mut out).unwrap(), None);
        assert_eq!(m.position(), Some(10_000 - 480));
        assert_eq!(m.poll(MediaTime::new(11_023, tb), &mut out).unwrap(), None);
        let b = m
            .poll(MediaTime::new(11_024, tb), &mut out)
            .unwrap()
            .unwrap();
        assert_eq!(b.frame, 9_520);
        assert_eq!(m.poll(MediaTime::new(11_024, tb), &mut out).unwrap(), None);
        // A clock in another time base works too: 0.3 s in 100 ns ticks is frame 14400.
        let hns = Timebase::new(1, 10_000_000).unwrap();
        let mut n = 0;
        while m
            .poll(MediaTime::new(3_000_000, hns), &mut out)
            .unwrap()
            .is_some()
        {
            n += 1;
        }
        // Blocks at 10544, 11568 and 12592 end at least 480 frames before 14400.
        assert_eq!(n, 3);
        assert_eq!(m.bus().stats().skipped_frames, 0);
        // A stall of 10 s: the mixer jumps to the newest block and counts the skip.
        let pos = m.position().unwrap();
        let b = m
            .poll(MediaTime::new(530_000, tb), &mut out)
            .unwrap()
            .unwrap();
        assert_eq!(b.frame, 530_000 - 480 - 1024);
        assert_eq!(m.bus().stats().skipped_frames, (b.frame - pos) as u64);
        assert!(matches!(
            m.poll(MediaTime::new(0, tb), &mut [0.0; 3]),
            Err(AudioError::OutputLength {
                expected: 2048,
                got: 3
            })
        ));
    }

    /// The control thread can add an input while the mixer thread is the one calling
    /// `mix`. The next block includes it, and removing it drops it from later blocks.
    #[test]
    fn an_input_added_while_the_mixer_runs_is_mixed_then_removed() {
        let mut mixer = Mixer::new(config(480)).unwrap();
        let ops = mixer.control();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut producer, control) = ops.add_input(stereo48()).unwrap();
            feed(&mut producer, 0.5, 0.5, 1.0);
            ready_tx.send(control).unwrap();
            let _ = done_rx.recv();
            producer
        });
        let control = ready_rx.recv().unwrap();
        let mut out = vec![0.0; 960];
        // The 8192-frame ring still holds frame 40000 of the second of audio.
        mixer.mix(40_000, &mut out).unwrap();
        assert!(
            out.iter().all(|sample| (*sample - 0.5).abs() < 1e-3),
            "the input added on another thread was not mixed"
        );
        assert!(control.remove());
        mixer.mix(40_480, &mut out).unwrap();
        assert!(
            out.iter().all(|sample| *sample == 0.0),
            "a removed input was still mixed"
        );
        assert!(
            !control.remove(),
            "a second remove should not find the input"
        );
        let _ = done_tx.send(());
        worker.join().unwrap();
    }

    #[test]
    fn the_mixer_refuses_more_inputs_than_it_reserved() {
        let mixer = Mixer::new(MixerConfig::default()).unwrap();
        let ops = mixer.control();
        let mut controls = Vec::new();
        for _ in 0..64 {
            controls.push(ops.add_input(stereo48()).unwrap().1);
        }
        assert!(matches!(
            ops.add_input(stereo48()),
            Err(AudioError::TooManyInputs)
        ));
        assert!(controls[0].remove());
        assert!(ops.add_input(stereo48()).is_ok());
    }

    /// With the control dropped, dropping the producer on another thread and then the
    /// mixer's reference frees the ring on the collector thread.
    #[test]
    fn a_removed_ring_is_freed_on_the_collector_thread() {
        let mut mixer = Mixer::new(config(480)).unwrap();
        let collector = mixer.plane.retirer().thread();
        let (producer, control) = mixer.add_input(stereo48()).unwrap();
        let (hook_tx, hook_rx) = std::sync::mpsc::channel();
        producer.ring().on_drop(hook_tx);
        let audio = std::thread::spawn(move || {
            let id = std::thread::current().id();
            drop(producer);
            id
        });
        let audio_id = audio.join().unwrap();
        assert!(
            hook_rx.try_recv().is_err(),
            "the ring was freed while the mixer still held it"
        );
        assert!(control.remove());
        // The control plane lets go of its reference first, so the mixer's is the last.
        drop(control);
        let mut out = vec![0.0; 960];
        mixer.mix(0, &mut out).unwrap();
        let freed_on = hook_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(freed_on, collector);
        assert_ne!(freed_on, audio_id);
        assert_ne!(freed_on, std::thread::current().id());
    }

    /// The case from #75 review N1: the mixer is dropped first, and the producer is
    /// dropped later on the audio thread. With no control handle left, the ring is
    /// still freed on the collector.
    #[test]
    fn dropping_the_mixer_first_still_frees_the_ring_on_the_collector() {
        let mut mixer = Mixer::new(config(480)).unwrap();
        let collector = mixer.plane.retirer().thread();
        let (producer, control) = mixer.add_input(stereo48()).unwrap();
        // Dropped early, so the producer's reference is the last.
        drop(control);
        let (hook_tx, hook_rx) = std::sync::mpsc::channel();
        producer.ring().on_drop(hook_tx);
        let mixer_id = std::thread::current().id();
        drop(mixer);
        assert!(
            hook_rx.try_recv().is_err(),
            "the ring was freed while the producer still held it"
        );
        let audio_id = std::thread::spawn(move || {
            let id = std::thread::current().id();
            drop(producer);
            id
        })
        .join()
        .unwrap();
        let freed_on = hook_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(freed_on, collector);
        assert_ne!(freed_on, audio_id);
        assert_ne!(freed_on, mixer_id);
    }
}
