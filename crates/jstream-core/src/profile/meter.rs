//! The drawing threads' side of `profile.get` (ADR-0103): a [`DrawMeter`] that times each
//! unit of a frame's work into maps it keeps from frame to frame, and publishes its
//! running totals once a second as one immutable [`DrawerTotals`].
//!
//! A steady frame allocates nothing: a unit seen before is found by reference, and only
//! a new one clones its key. The publish allocates the snapshot once a second; the
//! snapshot it replaces goes where the caller says (the render thread's collector,
//! R-RT-003). A unit not seen for [`FORGET_AFTER`] published seconds is dropped from the
//! maps, so a deleted item stops being carried.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use jstream_protocol::{SceneId, SourceId};

use crate::chain::ChainOwner;
use crate::snapshot::CaptureKey;

/// How often a meter publishes its totals.
pub(crate) const PUBLISH_EVERY: Duration = Duration::from_secs(1);

/// Published seconds a unit may go unseen before the meter forgets it.
const FORGET_AFTER: u32 = 60;

/// Where a frame's time went (`PhaseProfile`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Uploads = 0,
    Captures = 1,
    Chains = 2,
    Passes = 3,
    Finish = 4,
}

/// How many [`Phase`]s there are.
pub(crate) const PHASES: usize = 5;

/// Published seconds whose longest frame a meter remembers: more than the longest window
/// (`MAX_PROFILE_SECONDS`) and the sample before it.
pub(crate) const SECONDS_KEPT: usize = 64;

/// The longest frame of each of the last [`SECONDS_KEPT`] published seconds, published
/// second `n` (from 1) at `(n - 1) % SECONDS_KEPT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SecondMaxes(pub(crate) [u64; SECONDS_KEPT]);

impl Default for SecondMaxes {
    fn default() -> Self {
        Self([0; SECONDS_KEPT])
    }
}

impl SecondMaxes {
    /// The longest frame of the published seconds after `after` up to `upto`, as far
    /// back as the meter remembers.
    pub(crate) fn longest(&self, after: u64, upto: u64) -> u64 {
        let from = after.max(upto.saturating_sub(SECONDS_KEPT as u64));
        (from..upto)
            .map(|second| self.0[(second % SECONDS_KEPT as u64) as usize])
            .max()
            .unwrap_or(0)
    }
}

/// A unit of drawing work, by its owner, as the meter keeps it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum UnitKey {
    Upload(SourceId),
    Capture(CaptureKey),
    /// A video chain; an item's chain includes drawing its box.
    Chain(ChainOwner),
    Pass(SceneId),
    Composite,
}

/// One unit's running totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UnitTotals {
    /// Frames it ran in.
    pub(crate) frames: u64,
    /// The time it took in them.
    pub(crate) nanos: u64,
    /// The longest it took in one frame of the last published second.
    pub(crate) recent_max: u64,
}

/// A drawer's running totals since it started, as published once a second.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DrawerTotals {
    /// Numbers the drawer's runs, so totals of an earlier run are never subtracted from
    /// a later one's.
    pub(crate) run: u64,
    pub(crate) frames: u64,
    pub(crate) nanos: u64,
    /// The frames of each length, [`BUCKETS`] counts ([`histogram_bucket`]).
    pub(crate) histogram: Vec<u32>,
    /// The frames longer than the budget.
    pub(crate) over_budget: u64,
    /// The seconds published so far, this one included.
    pub(crate) seconds: u64,
    /// The longest frame of each recent published second.
    pub(crate) maxes: SecondMaxes,
    /// The ticks skipped (the program only).
    pub(crate) skipped: u64,
    pub(crate) phases: [u64; PHASES],
    pub(crate) units: Vec<(UnitKey, UnitTotals)>,
}

/// Where a drawer publishes, read by the control side.
#[derive(Default)]
pub(crate) struct DrawerSlot {
    current: ArcSwapOption<DrawerTotals>,
    /// Numbers the runs that publish here.
    runs: std::sync::atomic::AtomicU64,
}

impl DrawerSlot {
    /// The totals published last, if any.
    pub(crate) fn load(&self) -> Option<Arc<DrawerTotals>> {
        self.current.load_full()
    }

    /// A meter for a new run of the drawer, publishing here.
    pub(crate) fn meter(self: &Arc<Self>, budget: Duration) -> DrawMeter {
        let run = self
            .runs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .saturating_add(1);
        DrawMeter {
            slot: Some(Arc::clone(self)),
            run,
            budget,
            ..DrawMeter::default()
        }
    }
}

/// The bucket bounds of a frame-time histogram: a quarter millisecond each up to 16 ms,
/// a millisecond each up to 64 ms, 8 ms each up to 256 ms, then one for the rest.
const QUARTERS: usize = 64;
const MILLIS: usize = 48;
const EIGHTHS: usize = 24;
/// How many buckets a [`Histogram`] has.
pub(crate) const BUCKETS: usize = QUARTERS + MILLIS + EIGHTHS + 1;

/// Frame counts by length.
pub(crate) type Histogram = [u32; BUCKETS];

/// The bucket a frame of `nanos` falls in.
pub(crate) fn histogram_bucket(nanos: u64) -> usize {
    let quarter = 250_000;
    let milli = 1_000_000;
    if nanos < 16 * milli {
        (nanos / quarter) as usize
    } else if nanos < 64 * milli {
        QUARTERS + ((nanos - 16 * milli) / milli) as usize
    } else if nanos < 256 * milli {
        QUARTERS + MILLIS + ((nanos - 64 * milli) / (8 * milli)) as usize
    } else {
        BUCKETS - 1
    }
}

/// The upper edge of `bucket`, in nanoseconds; the last bucket has none, so its lower
/// edge.
pub(crate) fn bucket_top(bucket: usize) -> u64 {
    let quarter = 250_000u64;
    let milli = 1_000_000u64;
    let bucket = bucket as u64;
    let (quarters, millis, eighths) = (QUARTERS as u64, MILLIS as u64, EIGHTHS as u64);
    if bucket < quarters {
        (bucket + 1) * quarter
    } else if bucket < quarters + millis {
        16 * milli + (bucket - quarters + 1) * milli
    } else if bucket < quarters + millis + eighths {
        64 * milli + (bucket - quarters - millis + 1) * 8 * milli
    } else {
        256 * milli
    }
}

/// One unit's counters on the drawing thread.
#[derive(Clone, Copy, Debug, Default)]
struct Acc {
    /// Its time in the frame being drawn.
    frame: u64,
    /// Whether it ran in the frame being drawn.
    ran: bool,
    totals: UnitTotals,
    /// The longest frame of the second being counted.
    second_max: u64,
    /// Published seconds since it last ran.
    idle: u32,
}

impl Acc {
    fn add(&mut self, took: u64) {
        self.frame = self.frame.saturating_add(took);
        self.ran = true;
    }

    /// Folds the frame's time into the totals.
    fn end_frame(&mut self) {
        if std::mem::take(&mut self.ran) {
            let frame = std::mem::take(&mut self.frame);
            self.totals.frames = self.totals.frames.saturating_add(1);
            self.totals.nanos = self.totals.nanos.saturating_add(frame);
            self.second_max = self.second_max.max(frame);
            self.idle = 0;
        }
    }

    /// Ends a published second; returns whether to keep the unit.
    fn end_second(&mut self) -> bool {
        self.totals.recent_max = std::mem::take(&mut self.second_max);
        if self.totals.recent_max == 0 {
            self.idle = self.idle.saturating_add(1);
        }
        self.idle <= FORGET_AFTER
    }
}

/// Adds `took` to the unit `key` of `map`, cloning the key only for a new unit.
fn add<K: Ord + Clone>(map: &mut BTreeMap<K, Acc>, key: &K, took: u64) {
    match map.get_mut(key) {
        Some(acc) => acc.add(took),
        None => map.entry(key.clone()).or_default().add(took),
    }
}

/// Times a drawing thread's frames and their units (ADR-0103). Owned by the thread that
/// draws; [`Self::default`] publishes nowhere (`video.snapshot`, tests).
pub(crate) struct DrawMeter {
    slot: Option<Arc<DrawerSlot>>,
    run: u64,
    budget: Duration,
    uploads: BTreeMap<SourceId, Acc>,
    captures: BTreeMap<CaptureKey, Acc>,
    chains: BTreeMap<ChainOwner, Acc>,
    passes: BTreeMap<SceneId, Acc>,
    composite: Acc,
    frames: u64,
    nanos: u64,
    histogram: Histogram,
    over_budget: u64,
    /// The longest frame of the second being counted.
    second_max: u64,
    seconds: u64,
    maxes: SecondMaxes,
    skipped: u64,
    phases: [u64; PHASES],
    /// The phases' time in the frame being drawn, folded into `phases` when it ends.
    frame_phases: [u64; PHASES],
    /// When the meter last published, or `None` before its first frame.
    published: Option<Instant>,
}

impl Default for DrawMeter {
    fn default() -> Self {
        Self {
            slot: None,
            run: 0,
            budget: Duration::ZERO,
            uploads: BTreeMap::new(),
            captures: BTreeMap::new(),
            chains: BTreeMap::new(),
            passes: BTreeMap::new(),
            composite: Acc::default(),
            frames: 0,
            nanos: 0,
            histogram: [0; BUCKETS],
            over_budget: 0,
            second_max: 0,
            seconds: 0,
            maxes: SecondMaxes::default(),
            skipped: 0,
            phases: [0; PHASES],
            frame_phases: [0; PHASES],
            published: None,
        }
    }
}

impl DrawMeter {
    /// Adds `took` to `phase` in the frame being drawn.
    pub(crate) fn phase(&mut self, phase: Phase, took: Duration) {
        let slot = &mut self.frame_phases[phase as usize];
        *slot = slot.saturating_add(nanos(took));
    }

    /// Adds the time since `since` to `phase`, and returns it.
    pub(crate) fn phase_since(&mut self, phase: Phase, since: Instant) -> Duration {
        let took = since.elapsed();
        self.phase(phase, took);
        took
    }

    /// Adds the time since `since` to the upload of `source`.
    pub(crate) fn upload(&mut self, source: &SourceId, since: Instant) {
        add(&mut self.uploads, source, nanos(since.elapsed()));
    }

    /// Adds the time since `since` to the capture `capture`.
    pub(crate) fn capture(&mut self, capture: &CaptureKey, since: Instant) {
        add(&mut self.captures, capture, nanos(since.elapsed()));
    }

    /// Adds the time since `since` to `owner`'s video chain (an item's includes drawing
    /// its box).
    pub(crate) fn chain(&mut self, owner: &ChainOwner, since: Instant) {
        add(&mut self.chains, owner, nanos(since.elapsed()));
    }

    /// Adds `took` to `scene`'s pass.
    pub(crate) fn pass(&mut self, scene: &SceneId, took: Duration) {
        add(&mut self.passes, scene, nanos(took));
    }

    /// Adds `took` to the program scene's composite.
    pub(crate) fn composite(&mut self, took: Duration) {
        self.composite.add(nanos(took));
    }

    /// Counts `skipped` ticks the drawer skipped.
    pub(crate) fn skip(&mut self, skipped: u64) {
        self.skipped = self.skipped.saturating_add(skipped);
    }

    /// Forgets the time counted in a frame that failed part-way, so it isn't folded
    /// into the next frame that draws.
    pub(crate) fn discard_frame(&mut self) {
        fn discard(acc: &mut Acc) {
            acc.frame = 0;
            acc.ran = false;
        }
        self.uploads.values_mut().for_each(discard);
        self.captures.values_mut().for_each(discard);
        self.chains.values_mut().for_each(discard);
        self.passes.values_mut().for_each(discard);
        discard(&mut self.composite);
        self.frame_phases = [0; PHASES];
    }

    /// Ends a frame that took `took` from start to finish: folds each unit's time into
    /// its totals and, once [`PUBLISH_EVERY`] has passed since the last publish,
    /// publishes the totals and hands the ones they replace to `retire`.
    pub(crate) fn end_frame(
        &mut self,
        took: Duration,
        now: Instant,
        retire: impl FnOnce(Arc<DrawerTotals>),
    ) {
        let took = nanos(took);
        self.frames = self.frames.saturating_add(1);
        self.nanos = self.nanos.saturating_add(took);
        let bucket = &mut self.histogram[histogram_bucket(took)];
        *bucket = bucket.saturating_add(1);
        if !self.budget.is_zero() && took > nanos(self.budget) {
            self.over_budget = self.over_budget.saturating_add(1);
        }
        self.second_max = self.second_max.max(took);
        for (total, frame) in self.phases.iter_mut().zip(&mut self.frame_phases) {
            *total = total.saturating_add(std::mem::take(frame));
        }
        self.uploads.values_mut().for_each(Acc::end_frame);
        self.captures.values_mut().for_each(Acc::end_frame);
        self.chains.values_mut().for_each(Acc::end_frame);
        self.passes.values_mut().for_each(Acc::end_frame);
        self.composite.end_frame();
        let published = *self.published.get_or_insert(now);
        if now.saturating_duration_since(published) >= PUBLISH_EVERY {
            self.published = Some(now);
            self.publish(retire);
        }
    }

    /// Publishes the totals now, if the meter has a slot.
    fn publish(&mut self, retire: impl FnOnce(Arc<DrawerTotals>)) {
        self.uploads.retain(|_, acc| acc.end_second());
        self.captures.retain(|_, acc| acc.end_second());
        self.chains.retain(|_, acc| acc.end_second());
        self.passes.retain(|_, acc| acc.end_second());
        self.composite.end_second();
        let at = (self.seconds % SECONDS_KEPT as u64) as usize;
        self.maxes.0[at] = std::mem::take(&mut self.second_max);
        self.seconds = self.seconds.saturating_add(1);
        let Some(slot) = &self.slot else {
            return;
        };
        let mut units = Vec::with_capacity(
            self.uploads.len() + self.captures.len() + self.chains.len() + self.passes.len() + 1,
        );
        let ran = |acc: &&Acc| acc.totals.frames > 0;
        units.extend(
            (self.uploads.iter().filter(|(_, acc)| ran(acc)))
                .map(|(id, acc)| (UnitKey::Upload(id.clone()), acc.totals)),
        );
        units.extend(
            (self.captures.iter().filter(|(_, acc)| ran(acc)))
                .map(|(key, acc)| (UnitKey::Capture(key.clone()), acc.totals)),
        );
        units.extend(
            (self.chains.iter().filter(|(_, acc)| ran(acc)))
                .map(|(owner, acc)| (UnitKey::Chain(owner.clone()), acc.totals)),
        );
        units.extend(
            (self.passes.iter().filter(|(_, acc)| ran(acc)))
                .map(|(scene, acc)| (UnitKey::Pass(scene.clone()), acc.totals)),
        );
        if ran(&&self.composite) {
            units.push((UnitKey::Composite, self.composite.totals));
        }
        let totals = Arc::new(DrawerTotals {
            run: self.run,
            frames: self.frames,
            nanos: self.nanos,
            histogram: self.histogram.to_vec(),
            over_budget: self.over_budget,
            seconds: self.seconds,
            maxes: self.maxes,
            skipped: self.skipped,
            phases: self.phases,
            units,
        });
        if let Some(old) = slot.current.swap(Some(totals)) {
            retire(old);
        }
    }
}

/// `duration` in whole nanoseconds, saturating.
pub(crate) fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}
