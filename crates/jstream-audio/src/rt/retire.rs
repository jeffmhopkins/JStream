//! The collector thread (R-RT-003).
//!
//! A ring is an `Arc` of a few hundred kilobytes of atomics. The mixer and the audio
//! thread each hold one. Whichever of them releases its reference does not free the
//! allocation: it hands the `Arc` to this thread, and the free happens here, including
//! when that reference was the last one.
//!
//! Each [`Mixer`](crate::Mixer) starts its own collector and passes a [`Retirer`] to
//! everything that hands objects to it: the mixer, its control plane and each input's
//! producer (R-RS-008: no global). The collector ends once every `Retirer` is gone and
//! it has freed what they sent, so a producer dropped after its mixer still has one.
//!
//! The queue is bounded and lock-free, so a hand-off does not allocate. The collector
//! polls the queue instead of blocking on it: a receiver blocked in `recv` would make
//! each `try_send` lock the channel's waker and wake it with a syscall, on the
//! real-time thread. With nothing waiting, `try_send` is a few atomics.

use std::sync::Arc;
use std::thread::JoinHandle;
#[cfg(test)]
use std::thread::ThreadId;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError, bounded};

use super::bus::{BusShared, MAX_BUSES};
use super::chain::{Chain, Kernel};
use super::cmd::{COMMAND_CAP, MAX_INPUTS, MasterKernels};
use super::input::InputShared;
use super::ring::FrameRing;
use super::tap::{MAX_OUTPUT_TAPS, Tap};
use crate::error::AudioError;

/// Objects that can wait to be freed. Every input sends at most two of each kind (one
/// from the mixer, one from the producer), every bus one (from the mixer), every input
/// and bus its chain (a bus also its master chain), every input its monitor block, every output tap itself, every
/// removed filter itself (a master filter one copy per bus), and the mixer applies at
/// most a command queue's worth of changes per block, so a few blocks' worth fits with
/// room to spare.
const QUEUE_CAP: usize =
    7 * MAX_INPUTS + 6 * MAX_BUSES + 2 * MAX_OUTPUT_TAPS + (4 + MAX_BUSES) * COMMAND_CAP;

/// How long the collector sleeps between drains. Freeing late costs only memory.
const POLL: Duration = Duration::from_millis(10);

enum Retired {
    Ring(Arc<FrameRing>),
    Shared(Arc<InputShared>),
    Bus(Arc<BusShared>),
    Chain(Box<Chain>),
    Block(Box<[[f32; 2]]>),
    Kernel(Box<Kernel>),
    Kernels(MasterKernels),
    Tap(Tap),
}

/// Hands shared objects to one mixer's collector thread. Cloned into each holder.
#[derive(Clone, Debug)]
pub(crate) struct Retirer {
    tx: Sender<Retired>,
    #[cfg(test)]
    thread: ThreadId,
}

impl Retirer {
    /// Starts a collector thread. Called on the thread that builds the mixer.
    ///
    /// # Errors
    /// [`AudioError::CollectorUnavailable`] when the thread cannot be spawned.
    pub(crate) fn spawn() -> Result<Self, AudioError> {
        // Detached: the thread ends by itself once every `Retirer` is gone.
        Self::start().map(|(retirer, _thread)| retirer)
    }

    fn start() -> Result<(Self, JoinHandle<()>), AudioError> {
        let (tx, rx) = bounded(QUEUE_CAP);
        let handle = std::thread::Builder::new()
            .name("jstream-audio-collector".to_owned())
            .spawn(move || collect(&rx))
            .map_err(|_| AudioError::CollectorUnavailable)?;
        let retirer = Self {
            tx,
            #[cfg(test)]
            thread: handle.thread().id(),
        };
        Ok((retirer, handle))
    }

    /// The thread that frees this mixer's retired objects. Tests use it to tell that
    /// free apart from the audio thread and the mixer thread.
    #[cfg(test)]
    pub(crate) fn thread(&self) -> ThreadId {
        self.thread
    }

    pub(crate) fn ring(&self, ring: Arc<FrameRing>) {
        self.hand_off(Retired::Ring(ring));
    }

    pub(crate) fn shared(&self, shared: Arc<InputShared>) {
        self.hand_off(Retired::Shared(shared));
    }

    pub(crate) fn bus(&self, bus: Arc<BusShared>) {
        self.hand_off(Retired::Bus(bus));
    }

    pub(crate) fn chain(&self, chain: Box<Chain>) {
        self.hand_off(Retired::Chain(chain));
    }

    /// An input's monitor block.
    pub(crate) fn block(&self, block: Box<[[f32; 2]]>) {
        self.hand_off(Retired::Block(block));
    }

    pub(crate) fn kernel(&self, kernel: Box<Kernel>) {
        self.hand_off(Retired::Kernel(kernel));
    }

    /// A master filter's list of copies, emptied or not.
    pub(crate) fn kernels(&self, kernels: MasterKernels) {
        self.hand_off(Retired::Kernels(kernels));
    }

    pub(crate) fn tap(&self, tap: Tap) {
        self.hand_off(Retired::Tap(tap));
    }

    fn hand_off(&self, item: Retired) {
        match self.tx.try_send(item) {
            Ok(()) => {}
            // The queue is sized above what a mixer can retire between drains, and the
            // collector only stops once every `Retirer` is gone. If it is full anyway, a
            // detached thread frees the object so this real-time thread does not.
            // Spawning allocates: this is the overflow path, never the steady state.
            Err(TrySendError::Full(item) | TrySendError::Disconnected(item)) => {
                let _ = std::thread::Builder::new()
                    .name("jstream-audio-collector-overflow".to_owned())
                    .spawn(move || drop(item));
            }
        }
    }
}

/// The collector's loop: free whatever arrived, sleep, and stop once every sender is
/// gone and the queue is empty.
fn collect(rx: &Receiver<Retired>) {
    loop {
        loop {
            match rx.try_recv() {
                Ok(Retired::Ring(ring)) => drop(ring),
                Ok(Retired::Shared(shared)) => drop(shared),
                Ok(Retired::Bus(bus)) => drop(bus),
                Ok(Retired::Chain(chain)) => drop(chain),
                Ok(Retired::Block(block)) => drop(block),
                Ok(Retired::Kernel(kernel)) => drop(kernel),
                Ok(Retired::Kernels(kernels)) => drop(kernels),
                Ok(Retired::Tap(tap)) => drop(tap),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A retired ring is freed on the collector, and the collector ends once the last
    /// `Retirer` is gone, so each mixer's collector goes away with it.
    #[test]
    fn the_collector_frees_on_its_thread_and_ends_with_its_last_retirer() {
        let (retirer, handle) = Retirer::start().unwrap();
        let other = retirer.clone();
        let ring = Arc::new(FrameRing::new(1024));
        let (tx, rx) = std::sync::mpsc::channel();
        ring.on_drop(tx);
        retirer.ring(ring);
        let freed_on = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(freed_on, retirer.thread());
        drop(retirer);
        assert!(!handle.is_finished(), "a clone still holds the collector");
        other.shared(Arc::new(InputShared::new()));
        drop(other);
        handle.join().unwrap();
    }
}
