//! `profile.get` (protocol 0.73, ADR-0103): where the engine's time goes, per drawing
//! thread, per unit of drawing work and its owner, per source and per thread.
//!
//! The drawing threads time their frames with a [`DrawMeter`] and publish running totals
//! once a second into a [`DrawerSlot`] ([`meter`]). The `jstream-sources` thread and the
//! planner add theirs into [`SourceMeters`] and the plan totals, under locks of their
//! own that no real-time thread takes. The [`Profiler`] reads all of them, and the
//! process's threads, into a [`Sample`] on each `stats.tick` and each `profile.get`, keeps
//! the samples for [`MAX_PROFILE_SECONDS`], and answers a window as the difference
//! between the newest sample and the one from that long ago.
//!
//! Docs: crates/jstream-core/README.md#design, docs/adr/0103-per-item-performance-profile.md

pub(crate) mod meter;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jstream_platform::{ThreadCpu, ThreadTime};
use jstream_protocol::{
    DEFAULT_PROFILE_SECONDS, DEFAULT_PROFILE_TOP, Drawer, DrawerProfile, Finite32,
    MAX_PROFILE_SECONDS, MAX_PROFILE_TOP, PhaseProfile, PlanProfile, ProfileInfo, ProfileUnit,
    SourceId, SourceProfile, ThreadProfile, UnitProfile,
};

use crate::chain::ChainOwner;
use crate::lock;

use meter::{BUCKETS, PHASES, UnitTotals, bucket_top, nanos};
pub(crate) use meter::{DrawMeter, DrawerSlot, DrawerTotals, Phase, UnitKey};

/// Samples closer together than this replace each other, so a client that asks often
/// doesn't fill the history.
const SAMPLE_SPACING: Duration = Duration::from_millis(500);

/// What one source cost outside the drawing threads, as running totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SourceTotals {
    paints: u64,
    paint_nanos: u64,
    looks: u64,
    look_nanos: u64,
    /// The last build: when it ended, and how long it took.
    build: Option<(Instant, u64)>,
}

/// What the `jstream-sources` thread and the planner add per source. Its lock is its
/// own: the planner takes it under the collection lock, the thread without it, and
/// nothing takes the collection lock while holding it.
#[derive(Default)]
pub(crate) struct SourceMeters {
    sources: Mutex<BTreeMap<SourceId, SourceTotals>>,
    plan: Mutex<PlanTotals>,
}

impl SourceMeters {
    /// Counts a plan of the program mix (and the previews' plans after it) that held
    /// the collection lock for `took`.
    pub(crate) fn planned(&self, took: Duration) {
        let took = nanos(took);
        let mut plan = lock(&self.plan);
        plan.plans = plan.plans.saturating_add(1);
        plan.nanos = plan.nanos.saturating_add(took);
        plan.max = plan.max.max(took);
    }

    /// The plan totals, starting a new longest plan.
    fn read_plan(&self) -> PlanTotals {
        let mut plan = lock(&self.plan);
        let read = *plan;
        plan.max = 0;
        read
    }

    /// Counts a paint of `source` that took `took`.
    pub(crate) fn painted(&self, source: &SourceId, took: Duration) {
        self.with(source, |totals| {
            totals.paints = totals.paints.saturating_add(1);
            totals.paint_nanos = totals.paint_nanos.saturating_add(nanos(took));
        });
    }

    /// Counts a plan asking `source` for its frame, which took `took`.
    pub(crate) fn looked(&self, source: &SourceId, took: Duration) {
        self.with(source, |totals| {
            totals.looks = totals.looks.saturating_add(1);
            totals.look_nanos = totals.look_nanos.saturating_add(nanos(took));
        });
    }

    /// Records that building `source` took `took`, ending now.
    pub(crate) fn built(&self, source: &SourceId, took: Duration) {
        self.with(source, |totals| {
            totals.build = Some((Instant::now(), nanos(took)));
        });
    }

    /// Forgets every source `keep` says no to: the deleted ones.
    pub(crate) fn retain(&self, keep: impl Fn(&SourceId) -> bool) {
        lock(&self.sources).retain(|id, _| keep(id));
    }

    fn with(&self, source: &SourceId, add: impl FnOnce(&mut SourceTotals)) {
        let mut sources = lock(&self.sources);
        match sources.get_mut(source) {
            Some(totals) => add(totals),
            None => add(sources.entry(source.clone()).or_default()),
        }
    }

    fn read(&self) -> BTreeMap<SourceId, SourceTotals> {
        lock(&self.sources).clone()
    }

    /// How many paints of `source` were counted, for tests.
    #[cfg(test)]
    pub(crate) fn paints(&self, source: &SourceId) -> u64 {
        lock(&self.sources)
            .get(source)
            .map_or(0, |totals| totals.paints)
    }
}

/// What planning the program mix cost, as running totals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PlanTotals {
    plans: u64,
    nanos: u64,
    /// The longest plan since the last sample.
    max: u64,
}

/// Everything the profiler read at one time.
#[derive(Clone)]
struct Sample {
    at: Instant,
    program: Option<Arc<DrawerTotals>>,
    preview: Option<Arc<DrawerTotals>>,
    sources: BTreeMap<SourceId, SourceTotals>,
    plan: PlanTotals,
    /// The longest plan between the sample before and this one.
    plan_max: u64,
    threads: Option<Vec<ThreadTime>>,
    cpu: Option<Duration>,
}

/// The engine's profiler (ADR-0103). Lives in the engine's shared state; every lock in it
/// is its own, never taken with the collection lock held except [`SourceMeters`]'s and
/// the plan's, which nothing holds while taking another.
pub(crate) struct Profiler {
    started: Instant,
    program: Arc<DrawerSlot>,
    preview: Arc<DrawerSlot>,
    sources: Arc<SourceMeters>,
    threads: Mutex<Option<Arc<dyn ThreadCpu>>>,
    history: Mutex<VecDeque<Sample>>,
    /// The program's frame period, in nanoseconds. The control thread sets it when a
    /// stage opens; the stats thread reads it.
    budget_ns: std::sync::atomic::AtomicU64,
}

fn budget_nanos(budget: Duration) -> u64 {
    u64::try_from(budget.as_nanos()).unwrap_or(u64::MAX)
}

impl Profiler {
    /// A profiler whose drawers keep up at `budget` a frame, reading the per-source
    /// and plan totals from `sources`.
    pub(crate) fn new(budget: Duration, sources: Arc<SourceMeters>) -> Self {
        Self {
            started: Instant::now(),
            program: Arc::default(),
            preview: Arc::default(),
            sources,
            threads: Mutex::new(None),
            history: Mutex::new(VecDeque::new()),
            budget_ns: std::sync::atomic::AtomicU64::new(budget_nanos(budget)),
        }
    }

    fn budget(&self) -> Duration {
        Duration::from_nanos(self.budget_ns.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// The frame period the next stage's meters and `profile.get` use.
    pub(crate) fn set_budget(&self, budget: Duration) {
        self.budget_ns
            .store(budget_nanos(budget), std::sync::atomic::Ordering::Relaxed);
    }

    /// A meter for a new run of the program stage's render thread.
    pub(crate) fn program_meter(&self) -> DrawMeter {
        self.program.meter(self.budget())
    }

    /// A meter for a new run of the preview hub's thread.
    pub(crate) fn preview_meter(&self) -> DrawMeter {
        self.preview.meter(self.budget())
    }

    /// Reads each thread's CPU time from `threads` from now on.
    pub(crate) fn set_threads(&self, threads: Arc<dyn ThreadCpu>) {
        *lock(&self.threads) = Some(threads);
    }

    /// Takes a sample now, with the process's CPU time `cpu`. The stats threads call it
    /// once a second, outside the collection lock.
    pub(crate) fn sample(&self, cpu: Option<Duration>) {
        let sample = self.read(cpu);
        self.keep(sample);
    }

    /// The profile of the last `seconds` (or the default), its lists cut at `top`, with
    /// the process's CPU time now `cpu`. Takes a sample. Called outside the collection
    /// lock.
    pub(crate) fn profile(
        &self,
        seconds: Option<u32>,
        top: Option<u32>,
        cpu: Option<Duration>,
    ) -> ProfileInfo {
        let seconds = seconds
            .unwrap_or(DEFAULT_PROFILE_SECONDS)
            .clamp(1, MAX_PROFILE_SECONDS);
        let top = top.unwrap_or(DEFAULT_PROFILE_TOP).min(MAX_PROFILE_TOP) as usize;
        let mut now = self.read(cpu);
        let plan_max = now.plan_max;
        let base = {
            let history = lock(&self.history);
            let wanted = now
                .at
                .checked_sub(Duration::from_secs(u64::from(seconds)))
                .unwrap_or(self.started);
            // The newest sample at least `seconds` old, else the oldest there is.
            let base = history
                .iter()
                .rev()
                .find(|sample| sample.at <= wanted)
                .or_else(|| history.front())
                .filter(|sample| sample.at < now.at)
                .cloned();
            // The longest plan of every sample after the base.
            let since = base.as_ref().map(|base| base.at);
            let longest = history
                .iter()
                .filter(|sample| since.is_none_or(|since| sample.at > since))
                .map(|sample| sample.plan_max)
                .max()
                .unwrap_or(0);
            now.plan_max = now.plan_max.max(longest);
            base
        };
        let info = window(&now, base.as_ref(), self.started, self.budget(), top);
        // Kept with only its own longest plan, so a later window doesn't count the
        // earlier ones twice over a base after them.
        now.plan_max = plan_max;
        self.keep(now);
        info
    }

    fn read(&self, cpu: Option<Duration>) -> Sample {
        let reader = lock(&self.threads).clone();
        let threads = reader.and_then(|reader| reader.threads());
        let plan = self.sources.read_plan();
        Sample {
            at: Instant::now(),
            program: self.program.load(),
            preview: self.preview.load(),
            sources: self.sources.read(),
            plan,
            plan_max: plan.max,
            threads,
            cpu,
        }
    }

    fn keep(&self, sample: Sample) {
        let mut history = lock(&self.history);
        let keep_for = Duration::from_secs(u64::from(MAX_PROFILE_SECONDS) + 1);
        // One sample just before the window's start is kept, so a full window has a base.
        while history.len() > 1
            && history
                .get(1)
                .is_some_and(|next| sample.at.saturating_duration_since(next.at) > keep_for)
        {
            history.pop_front();
        }
        let crowded = history.len() > 1
            && history
                .back()
                .is_some_and(|last| sample.at.saturating_duration_since(last.at) < SAMPLE_SPACING);
        if crowded && let Some(last) = history.pop_back() {
            // The sample replaced keeps the longest plan it saw.
            let mut sample = sample;
            sample.plan_max = sample.plan_max.max(last.plan_max);
            history.push_back(sample);
        } else {
            history.push_back(sample);
        }
    }
}

/// The profile between `base` (or the start, with none) and `now`.
fn window(
    now: &Sample,
    base: Option<&Sample>,
    started: Instant,
    budget: Duration,
    top: usize,
) -> ProfileInfo {
    let since = base.map_or(started, |base| base.at);
    let span = now.at.saturating_duration_since(since);
    let seconds = span.as_secs_f32();
    let mut drawers = Vec::new();
    let mut units = Vec::new();
    for (drawer, current, before) in [
        (
            Drawer::Program,
            &now.program,
            base.and_then(|b| b.program.as_ref()),
        ),
        (
            Drawer::Preview,
            &now.preview,
            base.and_then(|b| b.preview.as_ref()),
        ),
    ] {
        let Some(current) = current else {
            continue;
        };
        // Totals of an earlier run of the drawer count from zero.
        let before = before.filter(|before| before.run == current.run);
        if let Some((profile, drawn)) = drawer_profile(drawer, current, before) {
            drawers.push(profile);
            units.extend(unit_profiles(drawer, current, before, drawn));
        }
    }
    units.sort_by_key(|unit| std::cmp::Reverse(unit.1));
    let units = units.into_iter().take(top).map(|(unit, _)| unit).collect();
    ProfileInfo {
        seconds: finite(seconds),
        frame_budget_ms: finite(budget.as_secs_f32() * 1000.0),
        gpu_timing: false,
        thread_cpu: now.threads.is_some(),
        drawers,
        units,
        sources: source_profiles(now, base, top),
        plan: plan_profile(now, base),
        process_cpu_pct: now.cpu.and_then(|cpu| {
            let before = base.map_or(Some(Duration::ZERO), |base| base.cpu)?;
            Some(percent(cpu.saturating_sub(before), span))
        }),
        threads: thread_profiles(now, base, span, top),
    }
}

/// A drawer's profile in the window, and the nanoseconds it drew for; `None` when it
/// drew no frame in it.
fn drawer_profile(
    drawer: Drawer,
    now: &DrawerTotals,
    before: Option<&Arc<DrawerTotals>>,
) -> Option<(DrawerProfile, u64)> {
    let frames = now.frames.saturating_sub(before.map_or(0, |b| b.frames));
    if frames == 0 {
        return None;
    }
    let drawn = now.nanos.saturating_sub(before.map_or(0, |b| b.nanos));
    let histogram: Vec<u32> = (0..BUCKETS)
        .map(|bucket| {
            let count = now.histogram.get(bucket).copied().unwrap_or(0);
            let earlier = before.map_or(0, |b| b.histogram.get(bucket).copied().unwrap_or(0));
            count.saturating_sub(earlier)
        })
        .collect();
    let total: u64 = histogram.iter().map(|count| u64::from(*count)).sum();
    // The bucket the frame at 95% of them falls in.
    let at = total.saturating_mul(95).div_ceil(100).max(1);
    let mut seen = 0u64;
    let mut p95 = 0;
    let mut top = 0;
    for (bucket, count) in histogram.iter().enumerate() {
        if *count == 0 {
            continue;
        }
        seen += u64::from(*count);
        if seen >= at && p95 == 0 {
            p95 = bucket_top(bucket);
        }
        top = bucket_top(bucket);
    }
    // The longest frame itself, from the seconds published since the base; the top of
    // its histogram bucket if the meter no longer remembers them.
    let longest = now
        .maxes
        .longest(before.map_or(0, |b| b.seconds), now.seconds);
    let max = if longest > 0 { longest } else { top };
    let phase = |at: usize| {
        let spent = now.phases[at].saturating_sub(before.map_or(0, |b| b.phases[at]));
        ms(spent / frames)
    };
    let phases = PhaseProfile {
        uploads_ms: phase(Phase::Uploads as usize),
        captures_ms: phase(Phase::Captures as usize),
        chains_ms: phase(Phase::Chains as usize),
        passes_ms: phase(Phase::Passes as usize),
        finish_ms: phase(Phase::Finish as usize),
    };
    debug_assert_eq!(PHASES, 5, "every phase is reported");
    Some((
        DrawerProfile {
            drawer,
            frames,
            frame_ms: ms(drawn / frames),
            frame_p95_ms: ms(p95),
            frame_max_ms: ms(max),
            over_budget: now
                .over_budget
                .saturating_sub(before.map_or(0, |b| b.over_budget)),
            skipped: now.skipped.saturating_sub(before.map_or(0, |b| b.skipped)),
            phases,
        },
        drawn,
    ))
}

/// The units a drawer ran in the window, each with its total time there.
fn unit_profiles(
    drawer: Drawer,
    now: &DrawerTotals,
    before: Option<&Arc<DrawerTotals>>,
    drawn: u64,
) -> Vec<(UnitProfile, u64)> {
    let earlier: BTreeMap<&UnitKey, &UnitTotals> = before
        .map(|b| b.units.iter().map(|(key, totals)| (key, totals)).collect())
        .unwrap_or_default();
    now.units
        .iter()
        .filter_map(|(key, totals)| {
            let before = earlier.get(key);
            let frames = totals.frames.saturating_sub(before.map_or(0, |b| b.frames));
            if frames == 0 {
                return None;
            }
            let spent = totals.nanos.saturating_sub(before.map_or(0, |b| b.nanos));
            let share = if drawn == 0 {
                0.0
            } else {
                (spent as f64 / drawn as f64).min(1.0) as f32
            };
            Some((
                UnitProfile {
                    drawer,
                    unit: unit(key),
                    frames,
                    ms: ms(spent / frames),
                    recent_max_ms: ms(totals.recent_max),
                    share: finite(share),
                    gpu_ms: None,
                },
                spent,
            ))
        })
        .collect()
}

/// The protocol's name for `key`.
fn unit(key: &UnitKey) -> ProfileUnit {
    match key {
        UnitKey::Upload(source_id) => ProfileUnit::Upload {
            source_id: source_id.clone(),
        },
        UnitKey::Capture(capture) => ProfileUnit::Capture {
            source_id: capture.source.clone(),
        },
        UnitKey::Chain(owner) => match owner {
            ChainOwner::Source(source_id) => ProfileUnit::SourceChain {
                source_id: source_id.clone(),
            },
            ChainOwner::Scene(scene_id) => ProfileUnit::SceneChain {
                scene_id: scene_id.clone(),
            },
            ChainOwner::Item(scene_id, item_id) => ProfileUnit::ItemEffects {
                scene_id: scene_id.clone(),
                item_id: item_id.clone(),
            },
            ChainOwner::Master(mix_id) => ProfileUnit::MasterChain {
                mix_id: mix_id.clone(),
            },
            ChainOwner::Output(output_id) => ProfileUnit::OutputChain {
                output_id: output_id.clone(),
            },
            ChainOwner::Placed(source_id, scene_id) => ProfileUnit::PlacedChain {
                source_id: source_id.clone(),
                scene_id: scene_id.clone(),
            },
        },
        UnitKey::Pass(scene_id) => ProfileUnit::ScenePass {
            scene_id: scene_id.clone(),
        },
        UnitKey::Composite => ProfileUnit::Composite,
    }
}

/// The sources that cost time in the window, costliest first.
fn source_profiles(now: &Sample, base: Option<&Sample>, top: usize) -> Vec<SourceProfile> {
    let since = base.map(|base| base.at);
    let mut sources: Vec<(SourceProfile, u64)> = now
        .sources
        .iter()
        .filter_map(|(source_id, totals)| {
            let before = base
                .and_then(|base| base.sources.get(source_id))
                .copied()
                .unwrap_or_default();
            let paints = totals.paints.saturating_sub(before.paints);
            let paint = totals.paint_nanos.saturating_sub(before.paint_nanos);
            let looks = totals.looks.saturating_sub(before.looks);
            let look = totals.look_nanos.saturating_sub(before.look_nanos);
            let build = totals
                .build
                .filter(|(at, _)| since.is_none_or(|since| *at > since))
                .map(|(_, took)| took);
            if paints == 0 && looks == 0 && build.is_none() {
                return None;
            }
            let cost = paint
                .saturating_add(look)
                .saturating_add(build.unwrap_or(0));
            Some((
                SourceProfile {
                    source_id: source_id.clone(),
                    paints,
                    paint_ms: ms(paint.checked_div(paints).unwrap_or(0)),
                    looks,
                    look_ms: ms(look.checked_div(looks).unwrap_or(0)),
                    build_ms: build.map(ms),
                },
                cost,
            ))
        })
        .collect();
    sources.sort_by_key(|source| std::cmp::Reverse(source.1));
    sources.into_iter().take(top).map(|(s, _)| s).collect()
}

fn plan_profile(now: &Sample, base: Option<&Sample>) -> PlanProfile {
    let before = base.map(|base| base.plan).unwrap_or_default();
    let plans = now.plan.plans.saturating_sub(before.plans);
    let spent = now.plan.nanos.saturating_sub(before.nanos);
    PlanProfile {
        plans,
        ms: ms(spent.checked_div(plans).unwrap_or(0)),
        recent_max_ms: ms(now.plan_max),
    }
}

/// The threads by name, busiest first. A thread is matched to its earlier reading by its
/// ID and name, so an ID the OS reused for a new thread counts from zero.
fn thread_profiles(
    now: &Sample,
    base: Option<&Sample>,
    span: Duration,
    top: usize,
) -> Vec<ThreadProfile> {
    let Some(threads) = &now.threads else {
        return Vec::new();
    };
    let earlier: BTreeMap<(u64, &str), Duration> = base
        .and_then(|base| base.threads.as_ref())
        .map(|threads| {
            threads
                .iter()
                .map(|t| ((t.id, t.name.as_str()), t.cpu))
                .collect()
        })
        .unwrap_or_default();
    let mut by_name: BTreeMap<&str, (u32, Duration)> = BTreeMap::new();
    for thread in threads {
        let before = earlier
            .get(&(thread.id, thread.name.as_str()))
            .copied()
            .unwrap_or(Duration::ZERO);
        let entry = by_name.entry(thread.name.as_str()).or_default();
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(thread.cpu.saturating_sub(before));
    }
    let mut named: Vec<(ThreadProfile, Duration)> = by_name
        .into_iter()
        .map(|(name, (count, used))| {
            let own = name.starts_with("jstream-") || name == "jstream";
            let name = (!name.is_empty()).then(|| clean(name));
            (
                ThreadProfile {
                    name: name.clone().filter(|_| own),
                    remote_name: name.filter(|_| !own),
                    count,
                    cpu_pct: percent(used, span),
                },
                used,
            )
        })
        .collect();
    named.sort_by_key(|thread| std::cmp::Reverse(thread.1));
    named.into_iter().take(top).map(|(t, _)| t).collect()
}

/// A thread name with control characters dropped and at most 64 characters.
fn clean(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(64).collect()
}

fn percent(used: Duration, span: Duration) -> Finite32 {
    if span.is_zero() {
        return Finite32::ZERO;
    }
    finite((used.as_secs_f64() / span.as_secs_f64() * 100.0) as f32)
}

/// `nanos` in milliseconds.
fn ms(nanos: u64) -> Finite32 {
    finite((nanos as f64 / 1_000_000.0) as f32)
}

fn finite(value: f32) -> Finite32 {
    Finite32::new(value).unwrap_or(Finite32::ZERO)
}

#[cfg(test)]
mod tests;
