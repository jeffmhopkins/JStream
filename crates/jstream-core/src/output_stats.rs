//! Each running output's live stats for `stats.tick` (protocol 0.59): what it has sent
//! and lost since its start, its bitrate over the last tick, and its health.
//!
//! The output thread counts what it hands its muxer into [`SentCounters`], and the
//! pipeline registers one [`OutputMeter`] per running output, whose `read` gathers that
//! with the output's drop counters. Like a [`DropSource`](crate::DropSource), `read`
//! only loads atomics or takes locks no real-time thread waits on. The engine's stats
//! thread samples every meter about once a second, outside the collection lock, and
//! keeps the last ten seconds of samples here to work out `kbps` and `health`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jstream_protocol::{OutputHealth, OutputId, OutputStatsInfo};

use crate::lock;

/// How far back `health` looks.
const WINDOW: Duration = Duration::from_secs(10);

/// The share of video frames lost over [`WINDOW`], in parts per ten thousand, at and
/// above which an output is `poor`: 2%.
const POOR_PER_10K: u64 = 200;

/// What the output thread has handed its muxer this run while the link was up.
#[derive(Debug, Default)]
pub(crate) struct SentCounters {
    bytes: AtomicU64,
    frames: AtomicU64,
}

impl SentCounters {
    /// Counts one packet of `bytes` the muxer took: a video frame when `video`.
    pub(crate) fn add(&self, bytes: usize, video: bool) {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        if video {
            self.frames.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The bytes and video frames counted so far.
    pub(crate) fn read(&self) -> (u64, u64) {
        (
            self.bytes.load(Ordering::Relaxed),
            self.frames.load(Ordering::Relaxed),
        )
    }
}

/// One reading of a running output's counters: running totals for this run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OutputSample {
    pub(crate) bytes: u64,
    pub(crate) frames: u64,
    pub(crate) dropped_frames: u64,
    pub(crate) dropped_packets: u64,
}

/// Reads one running output's counters.
pub(crate) type ReadSample = Box<dyn Fn() -> OutputSample + Send + Sync>;

/// One running output's meter and its recent samples.
struct OutputMeter {
    output_id: OutputId,
    started: Instant,
    read: ReadSample,
    /// The samples of the last [`WINDOW`], oldest first, and the one just before it.
    history: VecDeque<(Instant, OutputSample)>,
}

impl OutputMeter {
    /// Reads the counters at `now`, keeps the sample, and reports them.
    fn sample(&mut self, now: Instant) -> OutputStatsInfo {
        let sample = (self.read)();
        let previous = self.history.back().copied();
        self.history.push_back((now, sample));
        // Keep one sample at or before the window's start, so the window has a base.
        while self.history.len() > 2
            && self
                .history
                .get(1)
                .is_some_and(|(at, _)| now.saturating_duration_since(*at) >= WINDOW)
        {
            self.history.pop_front();
        }
        let (since, base) = previous.unwrap_or((self.started, OutputSample::default()));
        let kbps = kbps(sample.bytes.saturating_sub(base.bytes), now - since);
        // The window's base: the first sample of a run counts from zero.
        let oldest = match self.history.front() {
            Some((_, oldest)) if self.history.len() > 1 => *oldest,
            _ => OutputSample::default(),
        };
        OutputStatsInfo {
            output_id: self.output_id.clone(),
            uptime_ms: millis(now.saturating_duration_since(self.started)),
            bytes: sample.bytes,
            frames: sample.frames,
            kbps,
            dropped_frames: sample.dropped_frames,
            dropped_packets: sample.dropped_packets,
            health: health(&oldest, &sample),
        }
    }
}

/// Every running output's meter, on its own lock: taken by the stats thread before the
/// collection lock and never with it, and by the pipeline to add or remove a meter.
#[derive(Default)]
pub(crate) struct OutputMeters {
    next_id: u64,
    meters: BTreeMap<u64, OutputMeter>,
}

impl OutputMeters {
    /// Starts metering `output_id`'s run, which began at `started`.
    pub(crate) fn add(&mut self, output_id: OutputId, started: Instant, read: ReadSample) -> u64 {
        self.next_id = self.next_id.saturating_add(1);
        self.meters.insert(
            self.next_id,
            OutputMeter {
                output_id,
                started,
                read,
                history: VecDeque::new(),
            },
        );
        self.next_id
    }

    /// Stops metering. An unknown id does nothing.
    pub(crate) fn remove(&mut self, id: u64) {
        self.meters.remove(&id);
    }

    /// Samples every running output at `now`, by output ID.
    pub(crate) fn sample(&mut self, now: Instant) -> Vec<OutputStatsInfo> {
        let mut outputs: Vec<_> = self
            .meters
            .values_mut()
            .map(|meter| meter.sample(now))
            .collect();
        outputs.sort_by(|a, b| a.output_id.cmp(&b.output_id));
        outputs
    }
}

/// Removes its meter when dropped: held by the running output.
pub(crate) struct MeterGuard {
    meters: Arc<Mutex<OutputMeters>>,
    id: u64,
}

impl MeterGuard {
    pub(crate) fn new(meters: Arc<Mutex<OutputMeters>>, id: u64) -> Self {
        Self { meters, id }
    }
}

impl Drop for MeterGuard {
    fn drop(&mut self) {
        lock(&self.meters).remove(self.id);
    }
}

/// `bytes` over `elapsed` in kilobits a second, rounded to the nearest, or 0 over no
/// time.
fn kbps(bytes: u64, elapsed: Duration) -> u32 {
    let ms = elapsed.as_millis();
    if ms == 0 {
        return 0;
    }
    // bits / ms = kbit / s.
    let kbps = (u128::from(bytes) * 8 + ms / 2) / ms;
    u32::try_from(kbps).unwrap_or(u32::MAX)
}

/// The output's health from its counters at the window's start and now. The engine
/// makes a running output `poor` while its link reconnects, which this doesn't see.
fn health(start: &OutputSample, now: &OutputSample) -> OutputHealth {
    if now.frames == 0 {
        return OutputHealth::Waiting;
    }
    let lost = now.dropped_frames.saturating_sub(start.dropped_frames);
    let other = now.dropped_packets.saturating_sub(start.dropped_packets);
    let sent = now.frames.saturating_sub(start.frames);
    if lost == 0 && other == 0 {
        return OutputHealth::Good;
    }
    let total = sent.saturating_add(lost);
    if lost > 0 && lost.saturating_mul(10_000) >= total.saturating_mul(POOR_PER_10K) {
        OutputHealth::Poor
    } else {
        OutputHealth::Fair
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{OutputMeters, OutputSample, SentCounters, health, kbps};
    use jstream_protocol::{OutputHealth, OutputId};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn sample(frames: u64, dropped_frames: u64, dropped_packets: u64) -> OutputSample {
        OutputSample {
            bytes: 0,
            frames,
            dropped_frames,
            dropped_packets,
        }
    }

    #[test]
    fn kbps_is_bits_over_milliseconds() {
        assert_eq!(kbps(750_000, Duration::from_secs(1)), 6_000);
        assert_eq!(kbps(1_000, Duration::from_millis(1_500)), 5);
        assert_eq!(kbps(1_000, Duration::ZERO), 0);
    }

    #[test]
    fn health_follows_the_windows_losses() {
        let start = sample(1_000, 4, 2);
        // No video frame yet.
        assert_eq!(
            health(&sample(0, 0, 0), &sample(0, 0, 0)),
            OutputHealth::Waiting
        );
        // Nothing new lost in the window, though earlier drops are in the totals.
        assert_eq!(health(&start, &sample(1_600, 4, 2)), OutputHealth::Good);
        // One audio packet lost.
        assert_eq!(health(&start, &sample(1_600, 4, 3)), OutputHealth::Fair);
        // 11 of 600 frames: under 2%.
        assert_eq!(health(&start, &sample(1_589, 15, 2)), OutputHealth::Fair);
        // 12 of 600: 2%.
        assert_eq!(health(&start, &sample(1_588, 16, 2)), OutputHealth::Poor);
        // Everything lost: nothing sent in the window.
        assert_eq!(health(&start, &sample(1_000, 64, 2)), OutputHealth::Poor);
    }

    /// The meter reports running totals, the bitrate since the previous sample, and a
    /// health that forgets losses older than the window.
    #[test]
    fn a_meter_reports_rate_and_a_ten_second_health() {
        let sent = Arc::new(SentCounters::default());
        let dropped = Arc::new(Mutex::new(0u64));
        let mut meters = OutputMeters::default();
        let started = Instant::now();
        let read = {
            let (sent, dropped) = (Arc::clone(&sent), Arc::clone(&dropped));
            Box::new(move || {
                let (bytes, frames) = sent.read();
                OutputSample {
                    bytes,
                    frames,
                    dropped_frames: *dropped.lock().expect("lock"),
                    dropped_packets: 0,
                }
            })
        };
        let id = meters.add(OutputId::new("out_1"), started, read);
        let at = |s: u64| started + Duration::from_secs(s);

        let first = meters.sample(at(1));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].uptime_ms, 1_000);
        assert_eq!(first[0].kbps, 0);
        assert_eq!(first[0].health, OutputHealth::Waiting);

        // 60 frames of 12,500 bytes a second: 6 Mbps.
        let send = |frames: u64| {
            for _ in 0..frames {
                sent.add(12_500, true);
            }
        };
        send(60);
        let second = &meters.sample(at(2))[0];
        assert_eq!(second.kbps, 6_000);
        assert_eq!(second.frames, 60);
        assert_eq!(second.health, OutputHealth::Good);

        // A burst of drops, then ten clean seconds.
        send(30);
        *dropped.lock().expect("lock") = 30;
        assert_eq!(meters.sample(at(3))[0].health, OutputHealth::Poor);
        for s in 4..13 {
            send(60);
            assert_eq!(meters.sample(at(s))[0].health, OutputHealth::Poor, "{s}s");
        }
        send(60);
        let clean = &meters.sample(at(13))[0];
        assert_eq!(clean.health, OutputHealth::Good);
        assert_eq!(clean.dropped_frames, 30);
        assert_eq!(clean.uptime_ms, 13_000);

        meters.remove(id);
        assert!(meters.sample(at(14)).is_empty());
    }
}
