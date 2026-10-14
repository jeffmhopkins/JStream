//! Lock-free commands that add and remove mixer inputs while the mixer runs.
//!
//! The control thread allocates the ring and the producer, then `try_send`s the input
//! to the mixer. The mixer applies the command at the start of the next block and does
//! not allocate to do it (R-RT-001). The queue is bounded and lock-free
//! ([`crossbeam_channel`]).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crossbeam_channel::Sender;
use jstream_media::{AudioFormat, SampleFormat};

use super::input::{InputProducer, InputShared};
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
}

#[derive(Debug)]
pub(crate) struct ControlPlane {
    tx: Sender<Command>,
    next_id: AtomicU64,
    /// Ids accepted and not yet removed. Touched only from the control plane, never
    /// from the mixer thread.
    admitted: Mutex<BTreeSet<u64>>,
    sample_rate: u32,
    ring_frames: usize,
}

fn lock(mutex: &Mutex<BTreeSet<u64>>) -> std::sync::MutexGuard<'_, BTreeSet<u64>> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ControlPlane {
    pub(crate) fn new(tx: Sender<Command>, sample_rate: u32, ring_frames: usize) -> Self {
        Self {
            tx,
            next_id: AtomicU64::new(1),
            admitted: Mutex::new(BTreeSet::new()),
            sample_rate,
            ring_frames,
        }
    }

    /// Allocates an input and queues it for the mixer. The producer may push audio
    /// before the mixer has installed the input; the ring holds it.
    pub(crate) fn add(
        &self,
        format: AudioFormat,
    ) -> Result<(InputProducer, u64, Arc<InputShared>), AudioError> {
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
        let producer = InputProducer::new(format, engine, ring, Arc::clone(&shared));
        Ok((producer, id, shared))
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
}
