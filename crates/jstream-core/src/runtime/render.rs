//! The render snapshot: how the control side hands the program mix to the render
//! thread, and how the render thread says which one it holds
//! ([ARCH › Threads](../../../../docs/ARCHITECTURE.md#threading), "State to RT threads"
//! and "Live-value handover").
//!
//! The control side publishes an immutable [`RenderSnapshot`] through `arc-swap`, under
//! the collection lock, and only when what the program mix draws changed. The render
//! thread loads it without taking that lock. `video.snapshot` reads it with `rev` under
//! the lock and hands it to the snapshot thread, which never loads it. The render thread
//! stamps the generation it adopted in [`RenderHub`]'s reader slot, so the control side
//! can tell when a committed change is on screen; a snapshot it lets go of goes to the
//! pipeline's collector thread to be freed (R-RT-003).
//!
//! Beside the snapshot sits the render parameter channel: the live values of open
//! gestures ([`LiveParams`], ADR-0032), published the same way whenever they change.
//! The render thread reads it after the snapshot on every tick, so a live value is drawn
//! from the next tick with no new snapshot.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering, fence};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use arc_swap::ArcSwap;
use jstream_encode::{AudioInput, VideoInput};
use jstream_platform::GpuFrame;
use jstream_render::Reclaimed;

use crate::audio::{MonitorRoute, ScopeTap};
use crate::runtime::pipeline::VideoTap;
use crate::runtime::vcam::VcamTap;
use crate::snapshot::{LiveParams, Shot};

/// The reader slot's value while no render thread is adopting snapshots: the thread is
/// stopped, or never ran. A gesture waits on nothing while it is set.
const STOPPED: u64 = u64::MAX;

/// The reader slot's value once a thread has started but before it adopts a snapshot.
/// Lower than every generation, so nothing counts as caught up yet.
const STARTING: u64 = 0;

/// One published program mix and its render generation.
#[derive(Debug)]
pub(crate) struct RenderSnapshot {
    generation: u64,
    shot: Shot,
}

impl RenderSnapshot {
    /// The render generation it was published with, counting from 1.
    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// What it draws.
    pub(crate) fn shot(&self) -> &Shot {
        &self.shot
    }
}

/// The render subsystem's snapshot and its one reader slot. The engine keeps one, shared
/// with the program stage's render thread while any output runs.
pub(crate) struct RenderHub {
    current: ArcSwap<RenderSnapshot>,
    /// The parameter channel: the live values every reader applies to the snapshot.
    live: ArcSwap<LiveParams>,
    /// The generation the render thread adopted last, or [`STOPPED`] / [`STARTING`].
    adopted: AtomicU64,
}

impl Default for RenderHub {
    fn default() -> Self {
        Self {
            current: ArcSwap::from_pointee(RenderSnapshot {
                generation: 1,
                shot: Shot::default(),
            }),
            live: ArcSwap::from_pointee(LiveParams::default()),
            adopted: AtomicU64::new(STOPPED),
        }
    }
}

impl RenderHub {
    /// Publishes `shot` as the next generation, unless it draws what the current
    /// snapshot draws. Returns the generation that is current afterwards.
    ///
    /// Call it with the collection lock held: that lock is what makes the control side
    /// the one writer, so the comparison and the store cannot interleave with another
    /// publish. The replaced snapshot is released here, on the control side; the render
    /// thread, if it still holds it, hands its own reference to the collector.
    pub(crate) fn publish(&self, shot: Shot) -> u64 {
        let current = self.current.load();
        if current.shot == shot {
            return current.generation;
        }
        let generation = current.generation + 1;
        self.current
            .store(Arc::new(RenderSnapshot { generation, shot }));
        generation
    }

    /// The current snapshot, for a reader that is not a real-time thread.
    pub(crate) fn load(&self) -> Arc<RenderSnapshot> {
        self.current.load_full()
    }

    /// The render generation published last.
    pub(crate) fn generation(&self) -> u64 {
        self.current.load().generation
    }

    /// Publishes `live` on the parameter channel, unless it holds what the channel
    /// holds already. Called with the collection lock held, like [`publish`](Self::publish).
    pub(crate) fn publish_live(&self, live: LiveParams) {
        if **self.live.load() != live {
            self.live.store(Arc::new(live));
        }
    }

    /// The live values on the parameter channel, for a reader that is not a real-time
    /// thread.
    pub(crate) fn load_live(&self) -> Arc<LiveParams> {
        self.live.load_full()
    }

    /// Whether the render thread has adopted `generation` or a later one, or is not
    /// adopting snapshots at all (stopped, or never started). The control side polls
    /// this to release a gesture's live values (ADR-0032); the render thread never waits
    /// for it.
    ///
    /// The fence pairs with the one in [`RenderReader::start`]: the caller published
    /// the snapshot before asking, and the thread cleared its marker before loading, so
    /// at least one of them sees the other's store.
    pub(crate) fn caught_up(&self, generation: u64) -> bool {
        fence(Ordering::SeqCst);
        match self.adopted.load(Ordering::SeqCst) {
            STOPPED => true,
            adopted => adopted >= generation,
        }
    }
}

/// The render thread's handle on a [`RenderHub`]: the snapshot it holds, and its slot.
pub(crate) struct RenderReader {
    hub: Arc<RenderHub>,
    held: Option<Arc<RenderSnapshot>>,
    /// The live values it read last, loaded after `held`.
    live: Option<Arc<LiveParams>>,
    collector: Collector,
}

impl RenderReader {
    pub(crate) fn new(hub: Arc<RenderHub>, collector: Collector) -> Self {
        Self {
            hub,
            held: None,
            live: None,
            collector,
        }
    }

    /// Clears the stopped marker, then adopts the current snapshot. Called once when the
    /// render thread starts, before its first tick.
    pub(crate) fn start(&mut self) -> &RenderSnapshot {
        self.hub.adopted.store(STARTING, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        self.adopt()
    }

    /// The current snapshot. When the control side published a new one since the last
    /// call, this adopts it: stamps its generation in the slot (one atomic store) and
    /// hands the one it held to the collector. Otherwise it only compares a pointer. It
    /// never takes the collection lock or waits.
    ///
    /// Then it reads the parameter channel the same way, after the snapshot, as the
    /// live-value handover needs: once the control side has seen this generation and
    /// released a gesture's live values, the values read here no longer hold them, and
    /// the snapshot already shows the committed ones. [`held`](Self::held) returns both.
    pub(crate) fn adopt(&mut self) -> &RenderSnapshot {
        let guard = self.hub.current.load();
        let fresh = !self
            .held
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, &guard));
        if fresh {
            // The guard becomes a counted reference that `held` keeps, so it is never the
            // last one dropped here.
            let next = arc_swap::Guard::into_inner(guard);
            self.hub.adopted.store(next.generation, Ordering::SeqCst);
            if let Some(old) = self.held.replace(next) {
                self.collector.free(Garbage::Snapshot(old));
            }
        }
        let live = self.hub.live.load();
        let fresh = !self
            .live
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, &live));
        if fresh && let Some(old) = self.live.replace(arc_swap::Guard::into_inner(live)) {
            self.collector.free(Garbage::Live(old));
        }
        self.held
            .as_deref()
            .expect("invariant: the first adopt always stores a snapshot")
    }

    /// The snapshot and the live values the last [`adopt`](Self::adopt) read.
    pub(crate) fn held(&self) -> (&RenderSnapshot, &LiveParams) {
        let snapshot = self
            .held
            .as_deref()
            .expect("invariant: held is only called after adopt");
        let live = self
            .live
            .as_deref()
            .expect("invariant: adopt always stores live values");
        (snapshot, live)
    }

    /// GPU images the render thread's cache let go of, freed on the collector thread.
    pub(crate) fn free(&self, images: Reclaimed) {
        self.collector.free(Garbage::Images(images));
    }

    /// Publishes the stopped marker and lets go of the snapshot. Called when the render
    /// thread stops adopting snapshots; dropping the reader does it too.
    pub(crate) fn stop(&mut self) {
        self.hub.adopted.store(STOPPED, Ordering::SeqCst);
        if let Some(held) = self.held.take() {
            self.collector.free(Garbage::Snapshot(held));
        }
        if let Some(live) = self.live.take() {
            self.collector.free(Garbage::Live(live));
        }
    }
}

impl Drop for RenderReader {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What a real-time thread hands the collector instead of dropping it.
pub(crate) enum Garbage {
    /// A render snapshot the render thread replaced; it may hold the last reference.
    Snapshot(Arc<RenderSnapshot>),
    /// Live values the render thread replaced, for the same reason.
    Live(Arc<LiveParams>),
    /// GPU images whose last frame the GPU has finished.
    Images(Reclaimed),
    /// A finished encoder session's video input, let go of by the render thread. Once
    /// the session itself has dropped, it holds the last reference to the session's
    /// shared state and input channel.
    Video(VideoInput),
    /// The same for the audio input the mixer thread let go of.
    Audio(AudioInput),
    /// A frame buffer of a video mix the render thread stopped drawing.
    Pixels(Vec<u8>),
    /// The emptied list of encoder sessions such a mix fed, dropped here rather than
    /// on the render thread.
    Inputs(Vec<VideoTap>),
    /// The encode targets of such a mix. A session still encoding one of its frames
    /// holds that target until it is done.
    Targets(Vec<GpuFrame>),
    /// A virtual camera's tap the render thread let go of. Once its camera thread has
    /// ended, it holds the last reference to its channels and the output's stop flag.
    Vcam(VcamTap),
    /// A list of bus and source scopes the mixer thread replaced; it may hold the last
    /// reference to a scope no source draws any more.
    Scopes(Arc<Vec<ScopeTap>>),
    /// A monitor's feed the mixer thread let go of, once its device closed: it holds
    /// the monitor's ring and its closed flag.
    Monitor(MonitorRoute),
}

/// The sending side of a program stage's collector thread (R-RT-003): the render
/// thread's reader and the render and mixer threads each hold one. Sending moves the
/// object; the thread drops it. A send after the thread is gone drops it here, which
/// only happens during teardown.
#[derive(Clone)]
pub(crate) struct Collector {
    tx: Sender<Garbage>,
}

impl Collector {
    /// Starts `jstream-collector`. It runs until every [`Collector`] clone is dropped,
    /// then the returned handle joins.
    pub(crate) fn spawn() -> Result<(Self, JoinHandle<()>), String> {
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("jstream-collector".into())
            .spawn(move || collect(rx))
            .map_err(|err| format!("starting the jstream-collector thread: {err}"))?;
        Ok((Self { tx }, handle))
    }

    /// A collector with no thread: what it is given is dropped at once, where the caller
    /// is. For tests and for readers that are not real-time threads.
    #[cfg(test)]
    pub(crate) fn inline() -> Self {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        Self { tx }
    }

    pub(crate) fn free(&self, garbage: Garbage) {
        // An error hands the garbage back, and it drops here: the thread is gone.
        let _ = self.tx.send(garbage);
    }
}

fn collect(rx: Receiver<Garbage>) {
    for garbage in rx {
        match garbage {
            Garbage::Snapshot(snapshot) => drop(snapshot),
            Garbage::Live(live) => drop(live),
            Garbage::Images(images) => drop(images),
            Garbage::Video(input) => drop(input),
            Garbage::Audio(input) => drop(input),
            Garbage::Pixels(pixels) => drop(pixels),
            Garbage::Inputs(inputs) => drop(inputs),
            Garbage::Targets(targets) => drop(targets),
            Garbage::Vcam(tap) => drop(tap),
            Garbage::Scopes(taps) => drop(taps),
            Garbage::Monitor(route) => drop(route),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Collector, RenderHub, RenderReader};
    use crate::snapshot::{Frame, plan};
    use jstream_graph::Graph;
    use jstream_protocol::SourceId;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn frames(version: u64) -> BTreeMap<SourceId, Frame> {
        BTreeMap::from([(
            SourceId::new("cam"),
            Frame::new(1, 1, &[1, 2, 3, 4], version).expect("invariant: one pixel"),
        )])
    }

    /// A publish that draws the same thing keeps the generation; a change bumps it.
    #[test]
    fn only_a_change_publishes_a_generation() {
        let hub = RenderHub::default();
        let graph = Graph::default();
        assert_eq!(hub.load().generation(), 1);
        assert_eq!(hub.publish(plan(&graph, &BTreeMap::new())), 1);
        // No scene places `cam`, so its frame is not part of the mix.
        assert_eq!(hub.publish(plan(&graph, &frames(7))), 1);
        assert_eq!(hub.load().generation(), 1);
    }

    /// The reader stamps the generation it adopts, and a stopped reader counts as
    /// caught up with anything.
    #[test]
    fn the_reader_stamps_its_generation() {
        let hub = Arc::new(RenderHub::default());
        assert!(hub.caught_up(5), "nothing is reading yet");
        let mut reader = RenderReader::new(Arc::clone(&hub), Collector::inline());
        assert_eq!(reader.start().generation(), 1);
        assert!(hub.caught_up(1));
        assert!(!hub.caught_up(2));

        let first = Arc::as_ptr(&hub.load());
        let next = crate::snapshot::tests::one_scene_shot();
        assert_eq!(hub.publish(next), 2);
        assert!(!hub.caught_up(2), "published but not adopted");
        assert_eq!(reader.adopt().generation(), 2);
        assert!(hub.caught_up(2));
        assert_ne!(Arc::as_ptr(&hub.load()), first);

        reader.stop();
        assert!(
            hub.caught_up(u64::MAX - 1),
            "a stopped reader holds nothing up"
        );
    }

    /// A snapshot the reader lets go of reaches the collector thread, which frees it
    /// there, not on the reader's thread.
    #[test]
    fn a_replaced_snapshot_is_freed_on_the_collector() {
        let hub = Arc::new(RenderHub::default());
        let (collector, handle) = Collector::spawn().expect("collector");
        let mut reader = RenderReader::new(Arc::clone(&hub), collector);
        reader.start();
        let weak = Arc::downgrade(&hub.load());
        hub.publish(crate::snapshot::tests::one_scene_shot());
        reader.adopt();
        drop(reader);
        handle
            .join()
            .expect("the collector exits once its senders are gone");
        assert!(weak.upgrade().is_none(), "the first snapshot was freed");
    }
}
