//! Lock-free commands that add and remove mixer inputs and buses while the mixer runs.
//!
//! The control thread allocates the ring and the producer (or a bus's shared state),
//! then `try_send`s it to the mixer. The mixer applies the command at the start of the next block and does
//! not allocate to do it (R-RT-001). The queue is bounded and lock-free
//! ([`crossbeam_channel`]).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crossbeam_channel::Sender;
use jstream_media::{AudioFormat, SampleFormat};

use super::bus::{BusShared, MASTER_ID, MAX_BUSES};
use super::input::{InputProducer, InputShared};
use super::retire::Retirer;
use super::ring::FrameRing;
use super::sinc::{MAX_DOWN_RATIO, MAX_UP_RATIO};
use crate::error::AudioError;

/// Inputs the mixer can hold without growing its list on the mixer thread.
pub(crate) const MAX_INPUTS: usize = 64;

/// Pending adds and removes. Larger than [`MAX_INPUTS`] so a full set can be replaced
/// before the mixer has applied the removals.
pub(crate) const COMMAND_CAP: usize = 128;

pub(crate) enum Command {
    Add {
        id: u64,
        ring: Arc<FrameRing>,
        shared: Arc<InputShared>,
    },
    Remove(u64),
    AddBus {
        slot: usize,
        id: u64,
        shared: Arc<BusShared>,
    },
    RemoveBus {
        slot: usize,
        id: u64,
    },
}

#[derive(Debug)]
pub(crate) struct ControlPlane {
    tx: Sender<Command>,
    next_id: AtomicU64,
    /// Ids accepted and not yet removed. Touched only from the control plane, never
    /// from the mixer thread.
    admitted: Mutex<BTreeSet<u64>>,
    /// The id of the bus in each slot, as the control plane has queued it. Slot 0 is
    /// the master. Touched only from the control plane.
    buses: Mutex<[Option<u64>; MAX_BUSES]>,
    next_bus_id: AtomicU64,
    retirer: Retirer,
    sample_rate: u32,
    ring_frames: usize,
    /// This mixer's tag, unique in the process and never 0, carried by its
    /// [`BusId`](crate::BusId)s so another mixer's id is refused (#228 N2).
    tag: u64,
}

/// The next mixer's tag. 0 is [`BusId::MASTER`](crate::BusId::MASTER)'s, which names
/// every mixer's master.
static NEXT_TAG: AtomicU64 = AtomicU64::new(1);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ControlPlane {
    pub(crate) fn new(
        tx: Sender<Command>,
        retirer: Retirer,
        sample_rate: u32,
        ring_frames: usize,
    ) -> Self {
        Self {
            tx,
            next_id: AtomicU64::new(1),
            admitted: Mutex::new(BTreeSet::new()),
            buses: Mutex::new(std::array::from_fn(|slot| (slot == 0).then_some(MASTER_ID))),
            next_bus_id: AtomicU64::new(MASTER_ID + 1),
            retirer,
            sample_rate,
            ring_frames,
            tag: NEXT_TAG.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// This mixer's tag.
    pub(crate) fn tag(&self) -> u64 {
        self.tag
    }

    /// Allocates an input and queues it for the mixer. The producer may push audio
    /// before the mixer has installed the input; the ring holds it.
    pub(crate) fn add(
        &self,
        format: AudioFormat,
    ) -> Result<(InputProducer, u64, Arc<InputShared>, Arc<FrameRing>), AudioError> {
        if format.sample_format() != SampleFormat::F32Interleaved {
            return Err(AudioError::UnsupportedFormat);
        }
        let rate = format.sample_rate();
        let engine = self.sample_rate;
        if u64::from(rate) > u64::from(engine) * u64::from(MAX_DOWN_RATIO)
            || u64::from(rate) * u64::from(MAX_UP_RATIO) < u64::from(engine)
        {
            return Err(AudioError::UnsupportedRate { rate, engine });
        }
        let mut admitted = lock(&self.admitted);
        if admitted.len() >= MAX_INPUTS {
            return Err(AudioError::TooManyInputs);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let ring = Arc::new(FrameRing::new(self.ring_frames));
        let shared = Arc::new(InputShared::new());
        let queued = Command::Add {
            id,
            ring: Arc::clone(&ring),
            shared: Arc::clone(&shared),
        };
        if self.tx.try_send(queued).is_err() {
            return Err(AudioError::CommandQueueFull);
        }
        admitted.insert(id);
        drop(admitted);
        let producer = InputProducer::new(
            format,
            engine,
            Arc::clone(&ring),
            Arc::clone(&shared),
            self.retirer.clone(),
        );
        Ok((producer, id, shared, ring))
    }

    /// The collector this mixer's objects are retired to.
    pub(crate) fn retirer(&self) -> &Retirer {
        &self.retirer
    }

    /// Queues removal of `id`. False when `id` is unknown or already removed, or when
    /// the command queue cannot take it (the input stays).
    pub(crate) fn remove(&self, id: u64) -> bool {
        let mut admitted = lock(&self.admitted);
        if !admitted.remove(&id) {
            return false;
        }
        if self.tx.try_send(Command::Remove(id)).is_err() {
            admitted.insert(id);
            return false;
        }
        true
    }

    /// Allocates a bus and queues it for the mixer. Returns its slot, its id and its
    /// shared state.
    pub(crate) fn add_bus(&self) -> Result<(usize, u64, Arc<BusShared>), AudioError> {
        let mut buses = lock(&self.buses);
        let Some(slot) = buses.iter().position(Option::is_none) else {
            return Err(AudioError::TooManyBuses);
        };
        let id = self.next_bus_id.fetch_add(1, Ordering::Relaxed);
        let shared = Arc::new(BusShared::new());
        let queued = Command::AddBus {
            slot,
            id,
            shared: Arc::clone(&shared),
        };
        if self.tx.try_send(queued).is_err() {
            return Err(AudioError::CommandQueueFull);
        }
        buses[slot] = Some(id);
        Ok((slot, id, shared))
    }

    /// Queues removal of bus `id` in `slot`. False for the master, for a bus already
    /// removed, or when the command queue cannot take it (the bus stays).
    pub(crate) fn remove_bus(&self, slot: usize, id: u64) -> bool {
        if id == MASTER_ID {
            return false;
        }
        let mut buses = lock(&self.buses);
        if buses.get(slot).copied().flatten() != Some(id) {
            return false;
        }
        if self.tx.try_send(Command::RemoveBus { slot, id }).is_err() {
            return false;
        }
        buses[slot] = None;
        true
    }
}
