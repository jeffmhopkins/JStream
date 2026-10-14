//! The collector thread (R-RT-003).
//!
//! A ring is an `Arc` of a few hundred kilobytes of atomics. The mixer and the audio
//! thread each hold one. Whichever of them releases its reference does not free the
//! allocation: it hands the `Arc` to this thread, and the free happens here, including
//! when that reference was the last one.
//!
//! The queue is bounded and lock-free, so the hand-off does not allocate, lock or
//! block. The collector is started from [`Mixer::new`](crate::Mixer::new), on the
//! thread that builds the mixer, and then lives for the process.

use std::sync::Arc;
use std::sync::OnceLock;
#[cfg(test)]
use std::thread::ThreadId;

use crossbeam_channel::{Sender, TrySendError, bounded};

use super::input::InputShared;
use super::ring::FrameRing;

enum Retired {
    Ring(Arc<FrameRing>),
    Shared(Arc<InputShared>),
}

struct Collector {
    tx: Sender<Retired>,
    #[cfg(test)]
    thread: ThreadId,
}

fn collector() -> &'static Collector {
    static COLLECTOR: OnceLock<Collector> = OnceLock::new();
    COLLECTOR.get_or_init(|| {
        let (tx, rx) = bounded(1024);
        let (id_tx, id_rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("jstream-audio-collector".to_owned())
            .spawn(move || {
                let _ = id_tx.send(std::thread::current().id());
                while let Ok(item) = rx.recv() {
                    match item {
                        Retired::Ring(ring) => drop(ring),
                        Retired::Shared(shared) => drop(shared),
                    }
                }
            });
        // A collector that failed to start leaves the `Arc`s to be freed by whoever
        // still holds them. Mixer construction is the caller, not a real-time thread.
        let thread = if spawned.is_ok() {
            id_rx.recv().unwrap_or_else(|_| std::thread::current().id())
        } else {
            std::thread::current().id()
        };
        #[cfg(not(test))]
        let _started = thread;
        Collector {
            tx,
            #[cfg(test)]
            thread,
        }
    })
}

/// Starts the collector if it is not already running. Called while the mixer is built.
pub(crate) fn start() {
    let _ = collector();
}

/// The thread that frees retired rings. Tests use it to tell that free apart from the
/// audio thread and the mixer thread.
#[cfg(test)]
pub(crate) fn collector_thread() -> ThreadId {
    collector().thread
}

pub(crate) fn ring(ring: Arc<FrameRing>) {
    hand_off(Retired::Ring(ring));
}

pub(crate) fn shared(shared: Arc<InputShared>) {
    hand_off(Retired::Shared(shared));
}

fn hand_off(item: Retired) {
    match collector().tx.try_send(item) {
        Ok(()) => {}
        // The bounded queue is full, or the collector never started. A detached
        // thread frees the object so this real-time thread does not. Spawning
        // allocates, which is the overflow path, not the steady state.
        Err(TrySendError::Full(item) | TrySendError::Disconnected(item)) => {
            let _ = std::thread::Builder::new()
                .name("jstream-audio-collector-overflow".to_owned())
                .spawn(move || drop(item));
        }
    }
}
