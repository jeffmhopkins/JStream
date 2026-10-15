//! Drop counters for `stats.tick` (R-RT-004).
//!
//! Each stage already counts its own drops. This module does not count again: a
//! [`DropSource`] reads a stage's running total, and [`Engine::emit_stats_tick`](crate::Engine::emit_stats_tick)
//! sums the sources into one event. The media pipeline registers a source per stage
//! when it builds the chain. Until then a host, or a test, can register the same
//! sources against the stages and fakes that exist now.
//!
//! A source's `drops` runs on the control thread. It must only load atomics (or take
//! locks no real-time thread waits on), must not call back into the engine, and must
//! not panic.

use crate::lock;
use jstream_audio::{BusControl, BusStats, InputControl, InputStats};
use jstream_encode::{AudioSession, SessionStats, VideoSession};
use jstream_platform::CaptureStats;
use jstream_protocol::DropCounts;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Reads one stage's running drop totals.
///
/// Called on the control thread when a `stats.tick` is taken. The pipeline registers
/// one source per stage; two sources that report the same drop would add it twice.
pub trait DropSource: Send + Sync {
    /// Running totals since this stage started. Fields it does not count stay 0.
    fn drops(&self) -> DropCounts;
}

/// A [`DropSource`] built from a closure, for a stage whose handle the pipeline owns.
pub struct FnDropSource<F>(F);

impl<F> FnDropSource<F>
where
    F: Fn() -> DropCounts + Send + Sync,
{
    /// A source that reports whatever `read` returns.
    pub fn new(read: F) -> Self {
        Self(read)
    }
}

impl<F> DropSource for FnDropSource<F>
where
    F: Fn() -> DropCounts + Send + Sync,
{
    fn drops(&self) -> DropCounts {
        self.0()
    }
}

/// Identifies a source registered with an engine, so the pipeline can remove it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DropSourceId(u64);

/// The sources registered with one engine, and the totals of sources that were removed.
#[derive(Default)]
pub(crate) struct DropLedger {
    next_id: u64,
    sources: BTreeMap<u64, Arc<dyn DropSource>>,
    /// Last sample of every source that has been removed. Removing a finished stage
    /// must not make the running total fall.
    residual: DropCounts,
}

impl DropLedger {
    pub(crate) fn add(&mut self, source: impl DropSource + 'static) -> DropSourceId {
        self.next_id = self.next_id.saturating_add(1);
        let id = DropSourceId(self.next_id);
        self.sources.insert(id.0, Arc::new(source));
        id
    }

    /// Forgets `id`, keeping the total it reported. An unknown id does nothing.
    pub(crate) fn remove(&mut self, id: DropSourceId) {
        if let Some(source) = self.sources.remove(&id.0) {
            self.residual = self.residual.saturating_add(source.drops());
        }
    }

    pub(crate) fn sample(&self) -> DropCounts {
        self.sources.values().fold(self.residual, |total, source| {
            total.saturating_add(source.drops())
        })
    }
}

/// Lock-free cell a real-time thread publishes its running totals into.
///
/// Capture streams, the render loop and the output queue are not shared with the
/// control thread. The thread that owns them copies its counters here once per tick;
/// the control thread reads the cell when it emits `stats.tick`. [`publish`](Self::publish)
/// only stores atomics, so it never waits on the control plane (R-RT-003). A reader can
/// observe a mix of two publishes. Each field is a running total, so when the publisher
/// only increases them the mix lies between the two samples.
#[derive(Clone, Debug, Default)]
pub struct DropSnapshot {
    inner: Arc<Snapshot>,
}

#[derive(Debug, Default)]
struct Snapshot {
    capture_frames: AtomicU64,
    audio_frames: AtomicU64,
    render_ticks: AtomicU64,
    encode_frames: AtomicU64,
    encode_blocks: AtomicU64,
    output_packets: AtomicU64,
}

impl DropSnapshot {
    /// A cell whose totals are all zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the published totals with `drops`.
    pub fn publish(&self, drops: DropCounts) {
        let store = |slot: &AtomicU64, value: u64| slot.store(value, Ordering::Relaxed);
        store(&self.inner.capture_frames, drops.capture_frames);
        store(&self.inner.audio_frames, drops.audio_frames);
        store(&self.inner.render_ticks, drops.render_ticks);
        store(&self.inner.encode_frames, drops.encode_frames);
        store(&self.inner.encode_blocks, drops.encode_blocks);
        store(&self.inner.output_packets, drops.output_packets);
    }
}

impl DropSource for DropSnapshot {
    fn drops(&self) -> DropCounts {
        let load = |slot: &AtomicU64| slot.load(Ordering::Relaxed);
        DropCounts {
            capture_frames: load(&self.inner.capture_frames),
            audio_frames: load(&self.inner.audio_frames),
            render_ticks: load(&self.inner.render_ticks),
            encode_frames: load(&self.inner.encode_frames),
            encode_blocks: load(&self.inner.encode_blocks),
            output_packets: load(&self.inner.output_packets),
        }
    }
}

/// Video frames a capture mailbox replaced before the reader took them.
#[must_use]
pub fn capture_drops(stats: CaptureStats) -> DropCounts {
    DropCounts {
        capture_frames: stats.frames_skipped,
        ..DropCounts::default()
    }
}

/// Video frames an encoder session dropped on the way in, and packets its subscribers lost.
///
/// `dropped_queue_full` and `dropped_encoder_busy` are input drops. `subscriber_drops`
/// are output drops: each subscriber is one output's packet queue. Silence is not a
/// drop; a video session has none.
#[must_use]
pub fn video_encode_drops(stats: SessionStats) -> DropCounts {
    DropCounts {
        encode_frames: stats
            .dropped_queue_full
            .saturating_add(stats.dropped_encoder_busy),
        output_packets: stats.subscriber_drops,
        ..DropCounts::default()
    }
}

/// Audio blocks an encoder session dropped on the way in, and packets its subscribers lost.
///
/// Gap-fill silence (`silence_frames`) covers a drop so the stream stays in time. It is
/// not itself a drop.
#[must_use]
pub fn audio_encode_drops(stats: SessionStats) -> DropCounts {
    DropCounts {
        encode_blocks: stats
            .dropped_queue_full
            .saturating_add(stats.dropped_encoder_busy),
        output_packets: stats.subscriber_drops,
        ..DropCounts::default()
    }
}

/// Audio frames an input dropped by overwriting its ring or by arriving too late.
///
/// Underruns, device gaps and frames dropped before the input anchored are silence the
/// mixer plays, not frames it threw away, so they are not included.
#[must_use]
pub fn audio_input_drops(stats: InputStats) -> DropCounts {
    DropCounts {
        audio_frames: stats.overrun_frames.saturating_add(stats.late_frames),
        ..DropCounts::default()
    }
}

/// Engine frames the mixer skipped because it fell behind the clock.
#[must_use]
pub fn audio_bus_drops(stats: BusStats) -> DropCounts {
    DropCounts {
        audio_frames: stats.skipped_frames,
        ..DropCounts::default()
    }
}

/// Render ticks skipped because the render loop fell a whole period or more behind
/// the media clock (a slow composite, or the thread not being scheduled).
///
/// The pipeline's render loop skips to the current tick and publishes this running
/// total, beside the capture mailbox's skips, through its [`DropSnapshot`].
#[must_use]
pub fn render_tick_drops(ticks: u64) -> DropCounts {
    DropCounts {
        render_ticks: ticks,
        ..DropCounts::default()
    }
}

/// Packets an output's own queue dropped.
///
/// Distinct from [`video_encode_drops`] / [`audio_encode_drops`], which already count
/// the encoder fan-out's subscriber queue. The output queue in front of the network
/// is not built yet; the output thread will publish this count.
#[must_use]
pub fn output_packet_drops(packets: u64) -> DropCounts {
    DropCounts {
        output_packets: packets,
        ..DropCounts::default()
    }
}

/// A video encode session's counters, read without stopping its thread.
pub struct VideoEncodeDrops {
    session: Arc<VideoSession>,
}

impl VideoEncodeDrops {
    /// Reports `session`'s counters. The session is shared (`Arc`) with every output
    /// that uses it.
    #[must_use]
    pub fn new(session: Arc<VideoSession>) -> Self {
        Self { session }
    }
}

impl DropSource for VideoEncodeDrops {
    fn drops(&self) -> DropCounts {
        video_encode_drops(self.session.stats())
    }
}

/// An audio encode session's counters, read without stopping its thread.
pub struct AudioEncodeDrops {
    session: Arc<AudioSession>,
}

impl AudioEncodeDrops {
    /// Reports `session`'s counters.
    #[must_use]
    pub fn new(session: Arc<AudioSession>) -> Self {
        Self { session }
    }
}

impl DropSource for AudioEncodeDrops {
    fn drops(&self) -> DropCounts {
        audio_encode_drops(self.session.stats())
    }
}

/// One mixer input's overrun and late frames.
pub struct AudioInputDrops {
    input: InputControl,
}

impl AudioInputDrops {
    /// Reports `input`'s counters. The handle is cheap to clone; the audio thread is
    /// not disturbed.
    #[must_use]
    pub fn new(input: InputControl) -> Self {
        Self { input }
    }
}

impl DropSource for AudioInputDrops {
    fn drops(&self) -> DropCounts {
        audio_input_drops(self.input.stats())
    }
}

/// The mix bus's skipped frames.
pub struct AudioBusDrops {
    bus: BusControl,
}

impl AudioBusDrops {
    /// Reports `bus`'s counters.
    #[must_use]
    pub fn new(bus: BusControl) -> Self {
        Self { bus }
    }
}

impl DropSource for AudioBusDrops {
    fn drops(&self) -> DropCounts {
        audio_bus_drops(self.bus.stats())
    }
}

/// Removes its sources when the pipeline stops, latching their last totals.
pub(crate) struct DropGuard {
    ledger: Arc<Mutex<DropLedger>>,
    ids: Vec<DropSourceId>,
}

impl DropGuard {
    pub(crate) fn new(ledger: Arc<Mutex<DropLedger>>, ids: Vec<DropSourceId>) -> Self {
        Self { ledger, ids }
    }
}

impl Drop for DropGuard {
    fn drop(&mut self) {
        let mut ledger = lock_ledger(&self.ledger);
        for id in self.ids.drain(..) {
            ledger.remove(id);
        }
    }
}

/// The engine's ledger. Separate from the engine lock: sampling a source must not run
/// while that lock is held, and a source must not need it.
pub(crate) type Ledger = Mutex<DropLedger>;

/// Locks the ledger the way the engine locks its state.
pub(crate) fn lock_ledger(ledger: &Ledger) -> std::sync::MutexGuard<'_, DropLedger> {
    lock(ledger)
}

#[cfg(test)]
mod tests {
    use super::{
        audio_encode_drops, capture_drops, output_packet_drops, render_tick_drops,
        video_encode_drops,
    };
    use jstream_encode::SessionStats;
    use jstream_platform::CaptureStats;
    use jstream_protocol::DropCounts;

    #[test]
    fn stage_counters_map_onto_distinct_fields() {
        let capture = capture_drops(CaptureStats {
            frames_captured: 10,
            frames_skipped: 4,
            frames_undecodable: 0,
        });
        let video = video_encode_drops(SessionStats {
            dropped_queue_full: 2,
            dropped_encoder_busy: 3,
            subscriber_drops: 5,
            silence_frames: 100,
            ..SessionStats::default()
        });
        let audio = audio_encode_drops(SessionStats {
            dropped_queue_full: 1,
            subscriber_drops: 2,
            silence_frames: 48,
            ..SessionStats::default()
        });
        let sum = capture
            .saturating_add(video)
            .saturating_add(audio)
            .saturating_add(render_tick_drops(2))
            .saturating_add(output_packet_drops(9));
        assert_eq!(
            sum,
            DropCounts {
                capture_frames: 4,
                audio_frames: 0,
                render_ticks: 2,
                encode_frames: 5,
                encode_blocks: 1,
                output_packets: 16,
            }
        );
        assert_eq!(video.encode_blocks, 0);
        assert_eq!(audio.encode_frames, 0);
    }
}
