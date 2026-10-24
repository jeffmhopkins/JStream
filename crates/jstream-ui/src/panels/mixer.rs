//! The audio mixer (ADR-0028), after the prototype's (spec-panels.md 8): the master card
//! with its loudness and true peak, the channels folded under it, a row per audio source
//! with its meter, fader, dB field, mute and routing, and, folded under Buses, a strip
//! per bus with its meter, fader and mute. Levels come from `meters.tick`. Docked in a
//! side column, or flipped from its title bar, the channels are vertical strips instead.
//!
//! A fader shows the value under the pointer while it is dragged and sends one edit when
//! it is let go (or on each key press), so a drag is one undo step. Hearing the fader
//! while dragging needs live values (ADR-0032), which the protocol doesn't carry yet.
//!
//! Deleting a bus reads `delete.plan` first, as deleting a scene or source does, so the
//! strip shows which `waveform` or `spectrum` settings drawing the bus it clears.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/adr/0028-audio-model-v2.md

use crate::app::SessionContext;
use crate::dock::PanelId;
use crate::panels::confirm::{Confirm, Pending};
use crate::panels::inspector::Fold;
use crate::panels::number::NumberField;
use crate::store::plan_summary;
use crate::workspace::DockState;
use futures_util::StreamExt;
use jstream_control::protocol::{
    Action, AudioFollow, Batch, BusId, BusInfo, BusMeter, Call, CallResult, DeleteTarget, Edit,
    Event, Finite32, Loudness, MAX_BUSES, MAX_GAIN_DB, MIN_GAIN_DB, Query, QueryResult, Rev,
    SendInfo, SourceAudio, SourceId, SourceKind, SourceMeter, gain_db_problem,
};
use jstream_control::{Control, ControlError};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

/// One audio source as the mixer shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// The source.
    pub source_id: SourceId,
    /// Its display name.
    pub name: String,
    /// Its audio settings.
    pub audio: SourceAudio,
    /// Whether it is a nested scene, which has no fader, mute or sends of its own.
    pub is_scene: bool,
}

/// The mixer's part of the collection at one revision.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// The revision `state.get` read.
    pub rev: Rev,
    /// The audio sources, by name, then by ID.
    pub channels: Vec<Channel>,
    /// The buses, the master first, then by ID.
    pub buses: Vec<BusInfo>,
    /// Every send.
    pub sends: Vec<SendInfo>,
}

impl Snapshot {
    /// The channel for this source.
    pub fn channel(&self, source_id: &SourceId) -> Option<&Channel> {
        self.channels.iter().find(|c| &c.source_id == source_id)
    }

    /// The bus with this ID.
    pub fn bus(&self, bus_id: &BusId) -> Option<&BusInfo> {
        self.buses.iter().find(|b| &b.bus_id == bus_id)
    }

    /// The gain of the send from `source_id` into `bus_id`, if there is one.
    pub fn send_gain(&self, source_id: &SourceId, bus_id: &BusId) -> Option<f32> {
        self.sends
            .iter()
            .find(|s| &s.source_id == source_id && &s.bus_id == bus_id)
            .map(|s| s.gain_db.get())
    }

    /// Changes one channel's audio in place, after the engine accepted the edit, so the
    /// strip doesn't jump back until the re-read arrives.
    fn patch_channel(&mut self, source_id: &SourceId, patch: impl FnOnce(&mut SourceAudio)) {
        if let Some(channel) = self.channels.iter_mut().find(|c| &c.source_id == source_id) {
            patch(&mut channel.audio);
        }
    }

    /// Changes one bus in place, as [`Snapshot::patch_channel`] does.
    fn patch_bus(&mut self, bus_id: &BusId, patch: impl FnOnce(&mut BusInfo)) {
        if let Some(bus) = self.buses.iter_mut().find(|b| &b.bus_id == bus_id) {
            patch(bus);
        }
    }

    /// Sets or removes one send in place, as [`Snapshot::patch_channel`] does.
    fn patch_send(&mut self, source_id: &SourceId, bus_id: &BusId, gain_db: Option<Finite32>) {
        self.sends
            .retain(|s| !(&s.source_id == source_id && &s.bus_id == bus_id));
        if let Some(gain_db) = gain_db {
            self.sends.push(SendInfo {
                source_id: source_id.clone(),
                bus_id: bus_id.clone(),
                gain_db,
            });
        }
    }
}

/// Reads the audio sources, buses and sends (`state.get`).
pub async fn load(control: &dyn Control) -> Result<Snapshot, String> {
    let call = Call::Query {
        query: Query::StateGet,
    };
    let result = match control.call(call).await {
        Ok(response) => match response.result {
            CallResult::Query { result } => result,
            _ => return Err("the engine answered a query with something else".into()),
        },
        Err(error) => return Err(describe(&error)),
    };
    let QueryResult::StateGet {
        rev,
        sources,
        mut buses,
        sends,
        ..
    } = result
    else {
        return Err(format!(
            "the engine answered `state.get` with `{}`",
            result.name()
        ));
    };
    let mut channels: Vec<Channel> = sources
        .into_iter()
        .filter_map(|source| {
            let audio = source.audio?;
            Some(Channel {
                is_scene: matches!(source.kind, SourceKind::Scene { .. }),
                source_id: source.source_id,
                name: source.name,
                audio,
            })
        })
        .collect();
    channels.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.source_id.cmp(&b.source_id))
    });
    buses.sort_by_key(|b| (!b.bus_id.is_master(), b.bus_id.clone()));
    Ok(Snapshot {
        rev,
        channels,
        buses,
        sends,
    })
}

/// Sends one edit as a batch of one.
pub async fn edit(control: &dyn Control, edit: Edit) -> Result<(), String> {
    control
        .call(Call::Batch(Batch::single(edit)))
        .await
        .map(drop)
        .map_err(|error| describe(&error))
}

/// Reads the `delete.plan` for the bus `bus_id`, shown as `name`, as the confirmation
/// asks it: the plan clears the settings that draw the bus, then deletes it.
pub async fn plan_bus_delete(
    control: &dyn Control,
    bus_id: BusId,
    name: &str,
) -> Result<Pending, String> {
    let call = Call::Query {
        query: Query::DeletePlan {
            target: DeleteTarget::Bus { bus_id },
        },
    };
    let response = control.call(call).await.map_err(|error| describe(&error))?;
    match response.result {
        CallResult::Query {
            result: QueryResult::DeletePlan { rev, edits, .. },
        } => Ok(Pending {
            question: format!("Delete bus “{name}”?"),
            detail: plan_summary(&edits),
            rev,
            edits,
        }),
        CallResult::Query { result } => Err(format!(
            "the engine answered `delete.plan` with `{}`",
            result.name()
        )),
        _ => Err("the engine answered a query with something else".into()),
    }
}

/// Sends a confirmed plan as one batch, expecting the revision it was made at, so the
/// engine refuses it if the state changed in between.
pub async fn apply_plan(control: &dyn Control, plan: Pending) -> Result<(), String> {
    let batch = Batch {
        edits: plan.edits,
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: None,
    }
    .expecting(plan.rev);
    control
        .call(Call::Batch(batch))
        .await
        .map(drop)
        .map_err(|error| describe(&error))
}

/// A failed call, for the panel.
fn describe(error: &ControlError) -> String {
    error.to_string()
}

/// The first `bus-N` ID no existing bus has.
pub fn next_bus_id<'a>(existing: impl IntoIterator<Item = &'a BusId> + Clone) -> BusId {
    (1_u32..)
        .map(|n| BusId::new(format!("bus-{n}")))
        .find(|id| !existing.clone().into_iter().any(|taken| taken == id))
        .unwrap_or_else(|| BusId::new("bus"))
}

/// A gain in dB the engine accepts, or why not.
pub fn checked_gain(db: f32) -> Result<Finite32, String> {
    let gain = Finite32::new(db).ok_or_else(|| "Type a number of dB.".to_owned())?;
    match gain_db_problem(gain) {
        Some(problem) => Err(format!("The gain {problem}.")),
        None => Ok(gain),
    }
}

/// The fader's positions: a range input runs from 0 to this.
pub const FADER_STEPS: u16 = 1000;

/// The fader's taper: (position, dB) knots, joined by straight lines. Unity sits at
/// three quarters of the travel, and the quiet end is compressed, as on a console.
const TAPER: [(f32, f32); 6] = [
    (0.0, MIN_GAIN_DB),
    (100.0, -60.0),
    (300.0, -30.0),
    (500.0, -15.0),
    (750.0, 0.0),
    (1000.0, MAX_GAIN_DB),
];

/// Within this many dB of unity, a fader position snaps to 0 dB.
const UNITY_SNAP_DB: f32 = 0.25;

/// The taper's gain at a fader position, unrounded.
fn taper_db(position: u16) -> f32 {
    let p = f32::from(position.min(FADER_STEPS));
    TAPER
        .windows(2)
        .find(|pair| p <= pair[1].0)
        .map_or(MAX_GAIN_DB, |pair| {
            let ((p0, d0), (p1, d1)) = (pair[0], pair[1]);
            d0 + (d1 - d0) * (p - p0) / (p1 - p0)
        })
}

/// The gain at a fader position, rounded to 0.1 dB, snapping to 0 dB near unity.
pub fn position_to_db(position: u16) -> f32 {
    let db = (taper_db(position) * 10.0).round() / 10.0;
    if db.abs() < UNITY_SNAP_DB { 0.0 } else { db }
}

/// The fader position nearest a gain; gains outside the range sit at its ends. A
/// search over the positions rather than the taper's inverse, which would need a
/// float-to-integer cast.
pub fn db_to_position(db: f32) -> u16 {
    let db = db.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
    (0..=FADER_STEPS)
        .min_by(|&a, &b| {
            (taper_db(a) - db)
                .abs()
                .total_cmp(&(taper_db(b) - db).abs())
        })
        .unwrap_or(0)
}

/// A gain for a strip's readout: `+3.0 dB`, `0.0 dB`, `-12.5 dB`.
pub fn format_gain(db: f32) -> String {
    if db > 0.0 {
        format!("+{db:.1} dB")
    } else if db == 0.0 {
        "0.0 dB".into()
    } else {
        format!("{db:.1} dB")
    }
}

/// The quietest level a meter shows, in dBFS.
pub const METER_FLOOR_DBFS: f32 = -60.0;

/// A linear full-scale level in dBFS. Silence is `None`.
pub fn level_to_dbfs(level: f32) -> Option<f32> {
    (level > 0.0).then(|| 20.0 * level.log10())
}

/// How much of a meter a linear level fills, from 0 (at or below the floor) to 1 (at
/// or above 0 dBFS).
pub fn meter_fraction(level: f32) -> f32 {
    level_to_dbfs(level).map_or(0.0, |dbfs| {
        ((dbfs - METER_FLOOR_DBFS) / -METER_FLOOR_DBFS).clamp(0.0, 1.0)
    })
}

/// How many ticks (about a tenth of a second each) a peak is held before it falls.
pub const HOLD_TICKS: u8 = 15;

/// How much a held peak keeps per tick once it falls: about 3 dB a tick.
const HOLD_DECAY: f32 = 0.7;

/// A peak-hold marker for one meter channel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hold {
    /// The held level, linear.
    pub level: f32,
    /// Ticks left before it falls.
    pub ticks: u8,
}

impl Hold {
    /// Takes the next tick's peak: a louder one is held anew, else the held one waits,
    /// then falls.
    pub fn observe(&mut self, peak: f32) {
        if peak >= self.level {
            self.level = peak;
            self.ticks = HOLD_TICKS;
        } else if self.ticks > 0 {
            self.ticks -= 1;
        } else {
            self.level = (self.level * HOLD_DECAY).max(peak);
        }
    }
}

/// One strip's levels, from the last `meters.tick`, with its peak holds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    /// The peak since the previous tick, left then right, linear.
    pub peak: [f32; 2],
    /// The RMS level, left then right, linear.
    pub rms: [f32; 2],
    /// The peak holds, left then right.
    pub hold: [Hold; 2],
}

impl Reading {
    /// Takes a tick's levels, keeping the holds going.
    fn observe(&mut self, peak: [Finite32; 2], rms: [Finite32; 2]) {
        for ch in 0..2 {
            self.peak[ch] = peak[ch].get();
            self.rms[ch] = rms[ch].get();
            self.hold[ch].observe(self.peak[ch]);
        }
    }

    /// Whether either channel reached full scale within the hold.
    pub fn clipped(&self) -> bool {
        self.hold.iter().any(|h| h.level >= 1.0)
    }
}

/// The master's loudness readings, from the last `meters.tick`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MasterReadings {
    /// The true peak since the previous tick, left then right, linear.
    pub true_peak: Option<[f32; 2]>,
    /// Momentary, short-term and integrated loudness.
    pub loudness: Option<Loudness>,
}

/// Every strip's levels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeterBank {
    /// By source.
    pub sources: BTreeMap<SourceId, Reading>,
    /// By bus.
    pub buses: BTreeMap<BusId, Reading>,
    /// The master's loudness and true peak.
    pub master: MasterReadings,
}

impl MeterBank {
    /// Takes a `meters.tick`. A strip the tick leaves out (its source has no mixer
    /// input now) loses its reading.
    pub fn observe(&mut self, sources: &[SourceMeter], buses: &[BusMeter]) {
        let mut next = BTreeMap::new();
        for meter in sources {
            let mut reading = self.sources.remove(&meter.source_id).unwrap_or_default();
            reading.observe(meter.peak, meter.rms);
            next.insert(meter.source_id.clone(), reading);
        }
        self.sources = next;
        let mut next = BTreeMap::new();
        self.master = MasterReadings::default();
        for meter in buses {
            let mut reading = self.buses.remove(&meter.bus_id).unwrap_or_default();
            reading.observe(meter.peak, meter.rms);
            next.insert(meter.bus_id.clone(), reading);
            if meter.bus_id.is_master() {
                self.master = MasterReadings {
                    true_peak: meter.true_peak.map(|tp| [tp[0].get(), tp[1].get()]),
                    loudness: meter.loudness,
                };
            }
        }
        self.buses = next;
    }

    /// Forgets every reading: the ticks stopped, so the mixer isn't running.
    pub fn clear(&mut self) {
        *self = MeterBank::default();
    }
}

/// A loudness reading in LUFS for the master strip, or a dash before there is one.
pub fn format_lufs(value: Option<Finite32>) -> String {
    value.map_or_else(|| "–".into(), |v| minus(format!("{:.1}", v.get())))
}

/// A number with a true minus sign, as the prototype writes them.
fn minus(text: String) -> String {
    text.replace('-', "\u{2212}")
}

/// The master's true peak in dBTP: the louder channel.
pub fn format_true_peak(true_peak: Option<[f32; 2]>) -> String {
    match true_peak_dbtp(true_peak) {
        Some(dbtp) => minus(format!("{dbtp:.1} dBTP")),
        None => "– dBTP".into(),
    }
}

/// The loudness the master card's marker shows, in LUFS: the usual target for streams.
pub const LOUDNESS_TARGET_LUFS: f32 = -14.0;

/// The quietest and loudest the master card's loudness bar shows, in LUFS. The target
/// sits at 70% of it, where the prototype draws its marker.
const LOUDNESS_RANGE: (f32, f32) = (-60.0, 6.0);

/// How much of the master card's loudness bar a reading fills, from 0 to 1.
pub fn loudness_fraction(lufs: f32) -> f32 {
    let (floor, top) = LOUDNESS_RANGE;
    ((lufs - floor) / (top - floor)).clamp(0.0, 1.0)
}

/// The master's true peak in dBTP, the louder channel, if it isn't silent.
pub fn true_peak_dbtp(true_peak: Option<[f32; 2]>) -> Option<f32> {
    true_peak.and_then(|tp| level_to_dbfs(tp[0].max(tp[1])))
}

/// The note beside the master's true peak, and whether it warns: over 0 dBTP clips on
/// the way out.
pub fn true_peak_note(dbtp: Option<f32>) -> (&'static str, bool) {
    match dbtp {
        Some(dbtp) if dbtp > 0.0 => ("over 0 dBTP: add a true-peak limiter", true),
        Some(_) => ("safe", false),
        None => ("", false),
    }
}

/// "1 channel", "3 channels".
pub fn channel_count(n: usize) -> String {
    if n == 1 {
        "1 channel".into()
    } else {
        format!("{n} channels")
    }
}

/// The part of a channel's name before " · ", for a narrow vertical strip.
pub fn short_name(name: &str) -> &str {
    name.split(" · ").next().unwrap_or(name)
}

/// How the mixer draws its meters, set from its title bar (spec-panels.md M2): `None`
/// until then, following the dock.
#[derive(Clone, Copy)]
pub struct MixerOrientation(pub RwSignal<Option<bool>>);

impl MixerOrientation {
    /// Following the dock until the title bar sets it.
    pub fn new() -> Self {
        MixerOrientation(RwSignal::new(None))
    }
}

impl Default for MixerOrientation {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether the meters are vertical: as the title bar set them, or else when the mixer
/// is docked in a side column.
pub fn meters_vertical(set: Option<bool>, in_column: bool) -> bool {
    set.unwrap_or(in_column)
}

/// [`meters_vertical`] for the mixer as it is docked now.
fn vertical_now() -> Signal<bool> {
    let set = use_context::<MixerOrientation>();
    let dock = use_context::<DockState>();
    Signal::derive(move || {
        let in_column = dock.is_some_and(|dock| {
            dock.0.with(|d| {
                d.group_of(PanelId::Mixer)
                    .and_then(|g| d.place_of(g.id))
                    .is_some_and(|place| place.zone.vertical())
            })
        });
        meters_vertical(set.and_then(|s| s.0.get()), in_column)
    })
}

/// The mixer's title-bar button that flips its meters between horizontal and vertical
/// (spec-panels.md M2), drawn with the prototype's bars or lines.
#[component]
pub fn MixerFlip() -> impl IntoView {
    let Some(orientation) = use_context::<MixerOrientation>() else {
        return ().into_any();
    };
    let vertical = vertical_now();
    let title = move || {
        if vertical.get() {
            "Meters: vertical · click for horizontal"
        } else {
            "Meters: horizontal · click for vertical"
        }
    };
    let icon = move || {
        if vertical.get() {
            "M7 20V9 M12 20V4 M17 20V13"
        } else {
            "M4 7h11 M4 12h16 M4 17h7"
        }
    };
    view! {
        <button
            class="strip-tool"
            aria-label=title
            title=title
            on:pointerdown=move |event| event.stop_propagation()
            on:click=move |_| orientation.0.set(Some(!vertical.get_untracked()))
        >
            <svg class="icon" aria-hidden="true" viewBox="0 0 24 24" width="14" height="14">
                <path d=icon />
            </svg>
        </button>
    }
    .into_any()
}

/// How long without a `meters.tick` before the meters go blank.
const METERS_STALE: Duration = Duration::from_millis(500);

/// The panel's state, shared by its parts. Every field is `Copy`.
#[derive(Clone, Copy)]
struct Panel {
    /// The open session.
    session: SessionContext,
    /// The audio state, once read.
    snapshot: RwSignal<Option<Snapshot>>,
    /// The levels.
    meters: RwSignal<MeterBank>,
    /// The last failed call, shown until the next one succeeds or is dismissed.
    problem: RwSignal<Option<String>>,
    /// Bumped by each reload, so an older reload that finishes late is dropped.
    loads: StoredValue<u64>,
    /// Bumped by each `meters.tick`, so the blanking timer knows if another came.
    ticks: StoredValue<u64>,
}

impl Panel {
    /// Sends `edit`, then applies `patch` to the shown state if the engine took it.
    fn commit(
        self,
        change: Edit,
        patch: impl FnOnce(&mut Snapshot) + 'static,
    ) -> impl Future<Output = Result<(), String>> + 'static {
        let session = self.session.get();
        async move {
            let Some(session) = session else {
                return Err("Not connected.".into());
            };
            edit(&*session.control, change).await?;
            self.snapshot.try_update(|snapshot| {
                if let Some(snapshot) = snapshot.as_mut() {
                    patch(snapshot);
                }
            });
            Ok(())
        }
    }

    /// [`Panel::commit`], showing its error if it fails.
    fn apply(self, change: Edit, patch: impl FnOnce(&mut Snapshot) + 'static) {
        let done = self.commit(change, patch);
        spawn_local(async move {
            let outcome = done.await;
            self.problem.try_set(outcome.err());
        });
    }

    /// Turns a source's audio on or off, changes when it is heard, or mutes it.
    fn set_audio(
        self,
        source_id: SourceId,
        enabled: Option<bool>,
        follow: Option<AudioFollow>,
        muted: Option<bool>,
    ) {
        let patched = source_id.clone();
        self.apply(
            Edit::SourceSetAudio {
                source_id,
                enabled,
                follow,
                gain_db: None,
                muted,
            },
            move |s| {
                s.patch_channel(&patched, |a| {
                    a.enabled = enabled.unwrap_or(a.enabled);
                    a.follow = follow.unwrap_or(a.follow);
                    a.muted = muted.unwrap_or(a.muted);
                });
            },
        );
    }

    /// Sets a source's gain, if it is in range.
    fn commit_gain(
        self,
        source_id: SourceId,
        db: f32,
    ) -> impl Future<Output = Result<(), String>> + 'static {
        let gain = checked_gain(db);
        async move {
            let gain_db = gain?;
            let patched = source_id.clone();
            self.commit(
                Edit::SourceSetAudio {
                    source_id,
                    enabled: None,
                    follow: None,
                    gain_db: Some(gain_db),
                    muted: None,
                },
                move |s| s.patch_channel(&patched, |a| a.gain_db = gain_db),
            )
            .await
        }
    }

    /// Sends one action, showing its error if it fails.
    fn act(self, action: Action) {
        let Some(session) = self.session.get() else {
            return;
        };
        spawn_local(async move {
            let outcome = session
                .control
                .call(Call::Action { action })
                .await
                .map(drop)
                .map_err(|error| describe(&error));
            self.problem.try_set(outcome.err());
        });
    }

    /// Re-reads the audio state.
    fn reload(self) {
        let Some(session) = self.session.get() else {
            return;
        };
        let Some(mine) = self.loads.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        spawn_local(async move {
            let loaded = load(&*session.control).await;
            if self.loads.try_get_value() != Some(mine) {
                return;
            }
            match loaded {
                Ok(snapshot) => {
                    self.snapshot.try_set(Some(snapshot));
                }
                Err(problem) => {
                    self.problem.try_set(Some(problem));
                }
            }
        });
    }

    /// Takes a `meters.tick`, and blanks the meters if no other follows soon.
    fn tick(self, sources: &[SourceMeter], buses: &[BusMeter]) {
        self.meters.try_update(|bank| bank.observe(sources, buses));
        let Some(mine) = self.ticks.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        set_timeout(
            move || {
                if self.ticks.try_get_value() == Some(mine) {
                    self.meters.try_update(MeterBank::clear);
                }
            },
            METERS_STALE,
        );
    }
}

/// The audio mixer, with meters. Reads the open session from [`SessionContext`]
/// context.
#[component]
pub fn Mixer() -> impl IntoView {
    let Some(session) = use_context::<SessionContext>() else {
        return view! { <p class="placeholder">"Not connected."</p> }.into_any();
    };
    let panel = Panel {
        session,
        snapshot: RwSignal::new(None),
        meters: RwSignal::new(MeterBank::default()),
        problem: RwSignal::new(None),
        loads: StoredValue::new(0),
        ticks: StoredValue::new(0),
    };

    // Follow the engine: re-read on a collection change (or lost events), take levels
    // as they come. The loop holds only the event stream, which ends when the session
    // closes.
    if let Some(open) = session.get() {
        let mut events = open.control.events();
        drop(open);
        panel.reload();
        spawn_local(async move {
            while let Some(event) = events.next().await {
                match event {
                    Event::StateChanged { .. } | Event::EventsDropped { .. } => panel.reload(),
                    Event::MetersTick { sources, buses } => panel.tick(&sources, &buses),
                    _ => {}
                }
            }
        });
    }

    let channel_ids = move || {
        panel.snapshot.with(|s| {
            s.as_ref().map_or_else(Vec::new, |s| {
                s.channels.iter().map(|c| c.source_id.clone()).collect()
            })
        })
    };
    let bus_ids = move || {
        panel.snapshot.with(|s| {
            s.as_ref().map_or_else(Vec::new, |s| {
                s.buses.iter().map(|b| b.bus_id.clone()).collect()
            })
        })
    };
    let no_channels = move || {
        panel
            .snapshot
            .with(|s| s.as_ref().is_some_and(|s| s.channels.is_empty()))
    };
    // The channels fold under the master, folded at first (spec-panels.md M1).
    let open = RwSignal::new(false);
    // Strips side by side in a side column, or as the title bar sets (M2, M5).
    let vertical = vertical_now();
    let bus_count = Signal::derive(move || {
        let n = panel
            .snapshot
            .with(|s| s.as_ref().map_or(0, |s| s.buses.len()));
        if n == 1 {
            "1 bus".to_owned()
        } else {
            format!("{n} buses")
        }
    });

    view! {
        <section class="mixer-panel" class:vertical=vertical>
            {move || {
                panel
                    .problem
                    .get()
                    .map(|problem| {
                        view! {
                            <p class="problem" role="alert">
                                {problem}
                                " "
                                <button class="link" on:click=move |_| panel.problem.set(None)>
                                    "Dismiss"
                                </button>
                            </p>
                        }
                    })
            }}
            {move || {
                if vertical.get() {
                    view! {
                        <div class="mix-strips">
                            <MasterStrip panel=panel open=open />
                            <div class="mix-columns" id="mix-columns" hidden=move || !open.get()>
                                <div class="mix-divider" aria-hidden="true"></div>
                                <Show when=no_channels>
                                    <p class="placeholder">"No audio sources."</p>
                                </Show>
                                <ul class="mix-column-list" aria-label="Channels">
                                    <For each=channel_ids key=|id| id.clone() let:source_id>
                                        <ChannelColumn panel=panel source_id=source_id />
                                    </For>
                                </ul>
                            </div>
                        </div>
                    }
                    .into_any()
                } else {
                    view! {
                        <MasterCard panel=panel open=open />
                        <div class="mix-channels" id="mix-channels" hidden=move || !open.get()>
                            <Show when=no_channels>
                                <p class="placeholder">"No audio sources. Add an audio input, desktop audio or application audio source."</p>
                            </Show>
                            <ul class="mix-rows" aria-label="Channels">
                                <For each=channel_ids key=|id| id.clone() let:source_id>
                                    <ChannelStrip panel=panel source_id=source_id />
                                </For>
                            </ul>
                        </div>
                    }
                    .into_any()
                }
            }}
            <Fold title="Buses" open=false hint=bus_count>
                <ul class="strips">
                    <For each=bus_ids key=|id| id.clone() let:bus_id>
                        <BusStrip panel=panel bus_id=bus_id />
                    </For>
                </ul>
                <NewBusForm panel=panel />
            </Fold>
        </section>
    }
    .into_any()
}

/// The master card (spec-panels.md M3): the fold button over the channels with their
/// count, the monitor and program FX buttons the engine can't serve yet, the short-term
/// loudness against the target, and the true peak.
#[component]
fn MasterCard(panel: Panel, open: RwSignal<bool>) -> AnyView {
    let count = move || {
        channel_count(
            panel
                .snapshot
                .with(|s| s.as_ref().map_or(0, |s| s.channels.len())),
        )
    };
    let master = move || panel.meters.with(|m| m.master);
    let short = move || {
        master()
            .loudness
            .and_then(|l| l.short_term)
            .map(Finite32::get)
    };
    let fill = move || format!("{:.1}%", short().map_or(0.0, loudness_fraction) * 100.0);
    let target = format!("{:.1}%", loudness_fraction(LOUDNESS_TARGET_LUFS) * 100.0);
    let loudness_title = move || {
        let l = master().loudness.unwrap_or_default();
        format!(
            "Momentary {} · short-term {} · integrated {} LUFS",
            format_lufs(l.momentary),
            format_lufs(l.short_term),
            format_lufs(l.integrated),
        )
    };
    let dbtp = move || true_peak_dbtp(master().true_peak);
    let over = move || true_peak_note(dbtp()).1;
    view! {
        <div class="master-card" role="group" aria-label="Master">
            <div class="master-head">
                <button
                    class="master-fold"
                    aria-expanded=move || open.get().to_string()
                    aria-controls="mix-channels"
                    title="Show or hide the channels feeding the master"
                    on:click=move |_| open.update(|o| *o = !*o)
                >
                    <span class="caret" aria-hidden="true">{move || if open.get() { "▾" } else { "▸" }}</span>
                    <span class="master-name">"Master · program output"</span>
                    <span class="master-count">{count}</span>
                </button>
                <button
                    class="ghost-button"
                    title="Starts the integrated loudness over"
                    on:click=move |_| panel.act(Action::AudioResetLoudness {})
                >
                    "Reset"
                </button>
                <button
                    class="ghost-button"
                    disabled
                    title="What your headphones hear: the engine has no monitor output yet (decision D22)"
                >
                    "Hear: after FX"
                </button>
                <button
                    class="fx-button"
                    disabled
                    title="Filters on the program output: the master chain isn't built yet (FL-07)"
                >
                    "FX 0"
                </button>
            </div>
            <div class="master-row">
                <span class="master-label">"Loudness"</span>
                <div class="level-bar" title=loudness_title aria-hidden="true">
                    <div class="level-fill" style:width=fill></div>
                    <div class="level-target" title="Target" style:left=target></div>
                </div>
                <span class="master-value mono" role="status" title=loudness_title>
                    {move || format!("{} LUFS", format_lufs(master().loudness.and_then(|l| l.short_term)))}
                </span>
            </div>
            <div class="master-row" class:over=over>
                <span class="master-label">"True peak"</span>
                <span class="master-note">{move || true_peak_note(dbtp()).0}</span>
                <span class="master-value mono">{move || format_true_peak(master().true_peak)}</span>
            </div>
        </div>
    }
    .into_any()
}

/// The master strip of the vertical mixer (spec-panels.md M5): the fold button with the
/// channel count, the short-term loudness filling up to the target line, the loudness
/// and true peak, and the FX and monitor buttons the engine can't serve yet.
#[component]
fn MasterStrip(panel: Panel, open: RwSignal<bool>) -> AnyView {
    let count = move || {
        channel_count(
            panel
                .snapshot
                .with(|s| s.as_ref().map_or(0, |s| s.channels.len())),
        )
    };
    let master = move || panel.meters.with(|m| m.master);
    let short = move || master().loudness.and_then(|l| l.short_term);
    let fill = move || {
        format!(
            "{:.1}%",
            short().map_or(0.0, |l| loudness_fraction(l.get())) * 100.0
        )
    };
    let target = format!("{:.1}%", loudness_fraction(LOUDNESS_TARGET_LUFS) * 100.0);
    let dbtp = move || true_peak_dbtp(master().true_peak);
    view! {
        <div class="master-strip" role="group" aria-label="Master">
            <button
                class="master-fold"
                aria-expanded=move || open.get().to_string()
                aria-controls="mix-columns"
                title="Show or hide the channels feeding the master"
                on:click=move |_| open.update(|o| *o = !*o)
            >
                <span class="caret" aria-hidden="true">{move || if open.get() { "▾" } else { "▸" }}</span>
                "Master"
            </button>
            <span class="master-count">{count}</span>
            <div class="level-column" title="Loudness · white line = target" aria-hidden="true">
                <div class="level-fill" style:height=fill></div>
                <div class="level-target" style:bottom=target></div>
            </div>
            <span class="master-value mono" role="status">{move || format_lufs(short())}</span>
            <span class="master-unit">"LUFS"</span>
            <span
                class="master-value mono"
                class:over=move || true_peak_note(dbtp()).1
                title=move || format!("True peak: {}", true_peak_note(dbtp()).0)
            >
                {move || format_true_peak(master().true_peak).replace(" dBTP", "")}
            </span>
            <span class="master-unit">"dBTP"</span>
            <button
                class="fx-button"
                disabled
                title="Filters on the program output: the master chain isn't built yet (FL-07)"
            >
                "FX 0"
            </button>
            <button
                class="ghost-button"
                disabled
                title="What your headphones hear: the engine has no monitor output yet (decision D22)"
            >
                "Hear post"
            </button>
        </div>
    }
    .into_any()
}

/// A stereo meter: RMS bars, peak bars and peak holds, left over right, or side by side
/// and filling upwards when `vertical`.
fn meter(
    reading: impl Fn() -> Option<Reading> + Copy + Send + Sync + 'static,
    vertical: bool,
) -> AnyView {
    let percent = |fraction: f32| format!("{:.1}%", fraction * 100.0);
    let title = move || match reading().and_then(|r| level_to_dbfs(r.peak[0].max(r.peak[1]))) {
        Some(dbfs) => format!("Peak {dbfs:.1} dBFS"),
        None => "No signal".into(),
    };
    // How far a bar reaches, along the meter's axis only.
    let along = move |level: fn(&Reading, usize) -> f32, ch: usize, axis: bool| {
        move || {
            if axis == vertical {
                percent(reading().map_or(0.0, |r| meter_fraction(level(&r, ch))))
            } else {
                String::new()
            }
        }
    };
    let peak = |r: &Reading, ch: usize| r.peak[ch];
    let rms = |r: &Reading, ch: usize| r.rms[ch];
    let hold = |r: &Reading, ch: usize| r.hold[ch].level;
    let channel = move |ch: usize| {
        view! {
            <div class="meter-bar">
                <div
                    class="meter-peak"
                    style:width=along(peak, ch, false)
                    style:height=along(peak, ch, true)
                ></div>
                <div
                    class="meter-rms"
                    style:width=along(rms, ch, false)
                    style:height=along(rms, ch, true)
                ></div>
                <div
                    class="meter-hold"
                    class:clip=move || reading().is_some_and(|r| r.hold[ch].level >= 1.0)
                    style:left=along(hold, ch, false)
                    style:bottom=along(hold, ch, true)
                ></div>
            </div>
        }
    };
    view! {
        <div class="meter" class:vertical=vertical title=title aria-hidden="true">
            {channel(0)}
            {channel(1)}
        </div>
    }
    .into_any()
}

/// A gain the user is changing: the value to show, the dragged one while a drag holds
/// it, and the sender that commits one through `commit`, showing its error if it fails.
fn held_gain<Fut>(
    gain: impl Fn() -> f32 + Copy + Send + Sync + 'static,
    problem: RwSignal<Option<String>>,
    commit: impl Fn(f32) -> Fut + Copy + Send + Sync + 'static,
) -> (
    RwSignal<Option<f32>>,
    impl Fn() -> f32 + Copy + Send + Sync + 'static,
    impl Fn(f32) + Copy + Send + Sync + 'static,
)
where
    Fut: Future<Output = Result<(), String>> + 'static,
{
    let dragging = RwSignal::new(None::<f32>);
    let shown = move || dragging.get().unwrap_or_else(gain);
    let send = move |db: f32| {
        dragging.set(Some(db));
        let done = commit(db);
        spawn_local(async move {
            let outcome = done.await;
            dragging.try_set(None);
            problem.try_set(outcome.err());
        });
    };
    (dragging, shown, send)
}

/// A fader with its readout. It shows the dragged value while the pointer holds it, and
/// calls `commit` with the gain when it is let go and on each key press. Clicking the
/// readout sets 0 dB. (A double click on the fader would first move it to the pointer,
/// a second edit.)
fn fader<Fut>(
    label: impl Fn() -> String + Copy + Send + Sync + 'static,
    gain: impl Fn() -> f32 + Copy + Send + Sync + 'static,
    problem: RwSignal<Option<String>>,
    commit: impl Fn(f32) -> Fut + Copy + Send + Sync + 'static,
) -> AnyView
where
    Fut: Future<Output = Result<(), String>> + 'static,
{
    let (dragging, shown, send) = held_gain(gain, problem, commit);
    let position = |event: &leptos::ev::Event| {
        position_to_db(event_target_value(event).parse::<u16>().unwrap_or(0))
    };
    view! {
        <input
            type="range"
            class="fader"
            min="0"
            max=FADER_STEPS.to_string()
            step="1"
            aria-label=move || format!("{} fader", label())
            aria-valuetext=move || format_gain(shown())
            prop:value=move || db_to_position(shown()).to_string()
            on:input=move |event| dragging.set(Some(position(&event)))
            on:change=move |event| send(position(&event))
        />
        <button
            class="gain"
            title="Set to 0 dB"
            aria-label=move || format!("Set {} to 0 dB", label())
            on:click=move |_| send(0.0)
        >
            {move || format_gain(shown())}
        </button>
    }
    .into_any()
}

/// A channel's gain: the console-taper fader and the prototype's dB field (M4), the
/// field showing the fader's value while it is dragged. Either sends one edit through
/// `commit`.
fn channel_gain<Fut>(
    label: impl Fn() -> String + Copy + Send + Sync + 'static,
    gain: impl Fn() -> f32 + Copy + Send + Sync + 'static,
    problem: RwSignal<Option<String>>,
    commit: impl Fn(f32) -> Fut + Copy + Send + Sync + 'static,
) -> AnyView
where
    Fut: Future<Output = Result<(), String>> + 'static,
{
    let (dragging, shown, send) = held_gain(gain, problem, commit);
    let position = |event: &leptos::ev::Event| {
        position_to_db(event_target_value(event).parse::<u16>().unwrap_or(0))
    };
    let value = Signal::derive(move || Some(f64::from(shown())));
    let named = Signal::derive(move || format!("{} gain", label()));
    view! {
        <input
            type="range"
            class="fader"
            min="0"
            max=FADER_STEPS.to_string()
            step="1"
            aria-label=move || format!("{} fader", label())
            aria-valuetext=move || format_gain(shown())
            prop:value=move || db_to_position(shown()).to_string()
            on:input=move |event| dragging.set(Some(position(&event)))
            on:change=move |event| send(position(&event))
        />
        <NumberField
            named=named
            unit="dB"
            value=value
            step=0.5
            decimals=1
            fixed=true
            min=f64::from(MIN_GAIN_DB)
            max=f64::from(MAX_GAIN_DB)
            send=move |db: f64| {
                // The field keeps to the gain range at one decimal place.
                send(db as f32);
                true
            }
        />
    }
    .into_any()
}

/// A mute toggle.
fn mute_button(
    muted: impl Fn() -> bool + Copy + Send + Sync + 'static,
    toggle: impl Fn() + 'static,
) -> AnyView {
    view! {
        <button
            class="mute"
            class:on=muted
            aria-pressed=move || if muted() { "true" } else { "false" }
            on:click=move |_| toggle()
        >
            "Mute"
        </button>
    }
    .into_any()
}

/// One audio source's strip: its meter, fader, mute and, folded, its routing.
#[component]
fn ChannelStrip(panel: Panel, source_id: SourceId) -> AnyView {
    let channel = {
        let source_id = source_id.clone();
        Memo::new(move |_| {
            panel
                .snapshot
                .with(|s| s.as_ref().and_then(|s| s.channel(&source_id).cloned()))
        })
    };
    let id = StoredValue::new(source_id);
    let name = move || channel.with(|c| c.as_ref().map(|c| c.name.clone()).unwrap_or_default());
    let audio = move || channel.with(|c| c.as_ref().map(|c| c.audio));
    let is_scene = move || channel.with(|c| c.as_ref().is_some_and(|c| c.is_scene));
    let enabled = move || audio().is_some_and(|a| a.enabled);
    let muted = move || audio().is_some_and(|a| a.muted);
    let reading = move || id.with_value(|id| panel.meters.with(|m| m.sources.get(id).copied()));

    let set_audio =
        move |enabled: Option<bool>, follow: Option<AudioFollow>, muted: Option<bool>| {
            panel.set_audio(id.get_value(), enabled, follow, muted);
        };
    let commit_gain = move |db: f32| panel.commit_gain(id.get_value(), db);
    let choose_follow = move |event: leptos::ev::Event| {
        let follow = match event_target_value(&event).as_str() {
            "always" => AudioFollow::Always,
            _ => AudioFollow::WhenShown,
        };
        set_audio(None, Some(follow), None);
    };
    let follows_always = move || audio().is_some_and(|a| a.follow == AudioFollow::Always);
    let bus_ids = move || {
        panel.snapshot.with(|s| {
            s.as_ref().map_or_else(Vec::new, |s| {
                s.buses.iter().map(|b| b.bus_id.clone()).collect()
            })
        })
    };

    view! {
        <li class="mix-row" class:off=move || !enabled()>
            <span class="strip-name" title=move || id.with_value(ToString::to_string)>{name}</span>
            <Show
                when=move || !is_scene()
                fallback=|| view! { <span class="hint">"Nested scene: follows its own sources"</span> }
            >
                {meter(reading, false)}
                {channel_gain(
                    name,
                    move || audio().map_or(0.0, |a| a.gain_db.get()),
                    panel.problem,
                    commit_gain,
                )}
                {mute_button(muted, move || set_audio(None, None, Some(!muted())))}
            </Show>
            <details class="routing">
                <summary>"Routing"</summary>
                <label class="check">
                    <input
                        type="checkbox"
                        class="inline-check"
                        prop:checked=enabled
                        on:change=move |_| set_audio(Some(!enabled()), None, None)
                    />
                    "Audio on"
                </label>
                <label>
                    "Heard"
                    <select on:change=choose_follow>
                        <option value="always" prop:selected=follows_always>
                            "Always"
                        </option>
                        <option value="when_shown" prop:selected=move || !follows_always()>
                            "When shown on program"
                        </option>
                    </select>
                </label>
                <Show when=move || !is_scene()>
                    <ul class="sends">
                        <For each=bus_ids key=|id| id.clone() let:bus_id>
                            <SendRow panel=panel source_id=id.get_value() bus_id=bus_id />
                        </For>
                    </ul>
                </Show>
            </details>
        </li>
    }
    .into_any()
}

/// One audio source's strip in the vertical mixer (spec-panels.md M5): its short name, a
/// meter filling upwards, its gain stacked + over − and scrubbed up and down, and mute.
/// Its routing is in the horizontal mixer.
#[component]
fn ChannelColumn(panel: Panel, source_id: SourceId) -> AnyView {
    let channel = {
        let source_id = source_id.clone();
        Memo::new(move |_| {
            panel
                .snapshot
                .with(|s| s.as_ref().and_then(|s| s.channel(&source_id).cloned()))
        })
    };
    let id = StoredValue::new(source_id);
    let name = move || channel.with(|c| c.as_ref().map(|c| c.name.clone()).unwrap_or_default());
    let audio = move || channel.with(|c| c.as_ref().map(|c| c.audio));
    let is_scene = move || channel.with(|c| c.as_ref().is_some_and(|c| c.is_scene));
    let enabled = move || audio().is_some_and(|a| a.enabled);
    let muted = move || audio().is_some_and(|a| a.muted);
    let reading = move || id.with_value(|id| panel.meters.with(|m| m.sources.get(id).copied()));
    let (_, shown, send) = held_gain(
        move || audio().map_or(0.0, |a| a.gain_db.get()),
        panel.problem,
        move |db: f32| panel.commit_gain(id.get_value(), db),
    );
    let value = Signal::derive(move || Some(f64::from(shown())));
    let named = Signal::derive(move || format!("{} gain", name()));

    view! {
        <li class="mix-column" class:off=move || !enabled()>
            <span class="column-name" title=name>{move || short_name(&name()).to_owned()}</span>
            <Show
                when=move || !is_scene()
                fallback=|| view! { <span class="hint" title="Nested scene: follows its own sources">"Nested"</span> }
            >
                {meter(reading, true)}
                <NumberField
                    named=named
                    vertical=true
                    unit="dB"
                    value=value
                    step=0.5
                    decimals=1
                    fixed=true
                    min=f64::from(MIN_GAIN_DB)
                    max=f64::from(MAX_GAIN_DB)
                    send=move |db: f64| {
                        // The field keeps to the gain range at one decimal place.
                        send(db as f32);
                        true
                    }
                />
                {mute_button(muted, move || {
                    panel.set_audio(id.get_value(), None, None, Some(!muted()));
                })}
            </Show>
        </li>
    }
    .into_any()
}

/// One possible send from a source into a bus: on or off, and its gain.
#[component]
fn SendRow(panel: Panel, source_id: SourceId, bus_id: BusId) -> AnyView {
    let ids = StoredValue::new((source_id, bus_id));
    let gain = move || {
        ids.with_value(|(source_id, bus_id)| {
            panel
                .snapshot
                .with(|s| s.as_ref().and_then(|s| s.send_gain(source_id, bus_id)))
        })
    };
    let bus_name = move || {
        ids.with_value(|(_, bus_id)| {
            panel.snapshot.with(|s| {
                s.as_ref()
                    .and_then(|s| s.bus(bus_id))
                    .map_or_else(|| bus_id.to_string(), |b| b.name.clone())
            })
        })
    };
    let set = move |gain_db: Option<Finite32>| {
        let (source_id, bus_id) = ids.get_value();
        let (patch_source, patch_bus) = (source_id.clone(), bus_id.clone());
        let change = match gain_db {
            Some(gain_db) => Edit::SendSet {
                source_id,
                bus_id,
                gain_db,
            },
            None => Edit::SendRemove { source_id, bus_id },
        };
        panel.apply(change, move |s| {
            s.patch_send(&patch_source, &patch_bus, gain_db)
        });
    };
    let toggle = move |_| {
        set(if gain().is_some() {
            None
        } else {
            Some(Finite32::ZERO)
        })
    };
    let change_gain = move |event: leptos::ev::Event| match event_target_value(&event)
        .trim()
        .parse::<f32>()
        .map_err(|_| "Type a number of dB.".to_owned())
        .and_then(checked_gain)
    {
        Ok(gain_db) => set(Some(gain_db)),
        Err(problem) => panel.problem.set(Some(problem)),
    };
    view! {
        <li class="send">
            <label class="check">
                <input
                    type="checkbox"
                    class="inline-check"
                    prop:checked=move || gain().is_some()
                    on:change=toggle
                />
                {bus_name}
            </label>
            <input
                type="number"
                class="send-gain"
                min=MIN_GAIN_DB.to_string()
                max=MAX_GAIN_DB.to_string()
                step="0.5"
                aria-label=move || format!("Send to {} (dB)", bus_name())
                disabled=move || gain().is_none()
                prop:value=move || gain().map_or_else(String::new, |db| format!("{db:.1}"))
                on:change=change_gain
            />
            " dB"
        </li>
    }
    .into_any()
}

/// One bus's strip: its meter, fader and mute, renaming and deleting. The master's
/// loudness is on the master card.
#[component]
fn BusStrip(panel: Panel, bus_id: BusId) -> AnyView {
    let bus = {
        let bus_id = bus_id.clone();
        Memo::new(move |_| {
            panel
                .snapshot
                .with(|s| s.as_ref().and_then(|s| s.bus(&bus_id).cloned()))
        })
    };
    let is_master = bus_id.is_master();
    let id = StoredValue::new(bus_id);
    let name = move || bus.with(|b| b.as_ref().map(|b| b.name.clone()).unwrap_or_default());
    let muted = move || bus.with(|b| b.as_ref().is_some_and(|b| b.muted));
    let reading = move || id.with_value(|id| panel.meters.with(|m| m.buses.get(id).copied()));
    let renaming = RwSignal::new(false);
    let new_name = RwSignal::new(String::new());

    let commit_gain = move |db: f32| {
        let bus_id = id.get_value();
        let gain = checked_gain(db);
        async move {
            let gain_db = gain?;
            let patched = bus_id.clone();
            panel
                .commit(
                    Edit::BusSetFader {
                        bus_id,
                        gain_db: Some(gain_db),
                        muted: None,
                    },
                    move |s| s.patch_bus(&patched, |b| b.gain_db = gain_db),
                )
                .await
        }
    };
    let toggle_mute = move || {
        let bus_id = id.get_value();
        let patched = bus_id.clone();
        let muted = !muted();
        panel.apply(
            Edit::BusSetFader {
                bus_id,
                gain_db: None,
                muted: Some(muted),
            },
            move |s| s.patch_bus(&patched, |b| b.muted = muted),
        );
    };
    let begin_rename = move |_| {
        new_name.set(name());
        renaming.set(true);
    };
    let save_name = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let name = new_name.get_untracked().trim().to_owned();
        if name.is_empty() {
            panel.problem.set(Some("Type a name for the bus.".into()));
            return;
        }
        let bus_id = id.get_value();
        let patched = bus_id.clone();
        let renamed = name.clone();
        renaming.set(false);
        panel.apply(Edit::BusRename { bus_id, name }, move |s| {
            s.patch_bus(&patched, |b| b.name = renamed);
        });
    };
    let pending = RwSignal::new(None::<Pending>);
    let ask_delete = move |_| {
        let Some(session) = panel.session.get() else {
            return;
        };
        let bus_id = id.get_value();
        let shown = bus.with_untracked(|b| b.as_ref().map(|b| b.name.clone()));
        let name = shown.unwrap_or_else(|| bus_id.to_string());
        spawn_local(async move {
            match plan_bus_delete(&*session.control, bus_id, &name).await {
                Ok(plan) => {
                    pending.try_set(Some(plan));
                }
                Err(problem) => {
                    panel.problem.try_set(Some(problem));
                }
            }
        });
    };
    let confirm_delete = move |plan: Pending| {
        let Some(session) = panel.session.get() else {
            return;
        };
        let deleted = id.get_value();
        spawn_local(async move {
            let outcome = apply_plan(&*session.control, plan).await;
            if outcome.is_ok() {
                panel.snapshot.try_update(|snapshot| {
                    if let Some(s) = snapshot.as_mut() {
                        s.buses.retain(|b| b.bus_id != deleted);
                        s.sends.retain(|send| send.bus_id != deleted);
                    }
                });
            }
            panel.problem.try_set(outcome.err());
        });
    };
    view! {
        <li class="strip" class:master=is_master>
            <Show
                when=move || renaming.get()
                fallback=move || view! {
                    <span class="strip-name" title=move || id.with_value(ToString::to_string)>{name}</span>
                }
            >
                <form class="inline" on:submit=save_name>
                    <input type="text" aria-label="Bus name" bind:value=new_name />
                    <button type="submit">"Save"</button>
                    <button type="button" on:click=move |_| renaming.set(false)>"Cancel"</button>
                </form>
            </Show>
            {meter(reading, false)}
            {fader(
                name,
                move || bus.with(|b| b.as_ref().map_or(0.0, |b| b.gain_db.get())),
                panel.problem,
                commit_gain,
            )}
            {mute_button(muted, toggle_mute)}
            <details class="routing">
                <summary>"More"</summary>
                <button on:click=begin_rename>"Rename"</button>
                {(!is_master).then(|| view! { <button on:click=ask_delete>"Delete"</button> })}
            </details>
            <Confirm pending=pending confirm=confirm_delete />
        </li>
    }
    .into_any()
}

/// The form that adds a bus, with the next free `bus-N` ID.
#[component]
fn NewBusForm(panel: Panel) -> AnyView {
    let name = RwSignal::new(String::new());
    let full = move || {
        panel
            .snapshot
            .with(|s| s.as_ref().is_none_or(|s| s.buses.len() >= MAX_BUSES))
    };
    let add = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let Some(bus_id) = panel.snapshot.with_untracked(|s| {
            s.as_ref()
                .map(|s| next_bus_id(s.buses.iter().map(|b| &b.bus_id)))
        }) else {
            return;
        };
        let typed = name.get_untracked().trim().to_owned();
        let label = if typed.is_empty() {
            bus_id.to_string()
        } else {
            typed
        };
        name.set(String::new());
        panel.apply(
            Edit::BusCreate {
                bus_id,
                name: label,
            },
            |_| {},
        );
    };
    view! {
        <form class="inline new-bus" on:submit=add>
            <input type="text" aria-label="New bus name" placeholder="New bus name" bind:value=name />
            <button
                type="submit"
                disabled=full
                title=move || {
                    if full() {
                        format!("A collection holds at most {MAX_BUSES} buses")
                    } else {
                        String::new()
                    }
                }
            >
                "Add bus"
            </button>
        </form>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{
        FADER_STEPS, HOLD_TICKS, Hold, LOUDNESS_TARGET_LUFS, MeterBank, apply_plan, channel_count,
        checked_gain, db_to_position, edit, format_gain, format_lufs, format_true_peak, load,
        loudness_fraction, meter_fraction, meters_vertical, next_bus_id, plan_bus_delete,
        position_to_db, short_name, true_peak_dbtp, true_peak_note,
    };
    use jstream_control::protocol::{
        AudioFollow, Batch, BusId, BusMeter, Call, CallResult, DeleteTarget, Edit, Finite32, Hello,
        KindCategory, KindDescriptor, Loudness, PROTOCOL_VERSION, Query, QueryResult, Response,
        SceneId, Settings, SourceId, SourceKind, SourceMeter, Welcome,
    };
    use jstream_control::{BoxFuture, Control, ControlError, EventStream, MockEngine};
    use std::pin::pin;
    use std::sync::Mutex;
    use std::task::{Context, Poll, Waker};

    /// Polls a future that is ready at once, as the mock's are.
    fn now<F: Future>(future: F) -> F::Output {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the mock's futures are ready at once"),
        }
    }

    fn db(value: f32) -> Finite32 {
        Finite32::new(value).expect("finite")
    }

    fn audio_kind(kind_id: &str) -> KindDescriptor {
        KindDescriptor {
            kind_id: kind_id.into(),
            category: KindCategory::Source,
            name: kind_id.into(),
            version: 1,
            video: false,
            audio: true,
            params: Vec::new(),
        }
    }

    fn connected(engine: &MockEngine) -> impl Control {
        let control = engine.connect();
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        };
        now(control.hello(hello)).expect("hello");
        control
    }

    fn create_source(control: &impl Control, id: &str, name: &str, kind: SourceKind) {
        let create = Edit::SourceCreate {
            source_id: SourceId::new(id),
            name: name.into(),
            kind,
            settings: Default::default(),
            duplicated_from: None,
        };
        now(edit(control, create)).expect("create source");
    }

    /// The calls the panel makes, against the mock engine: sources and buses load, and
    /// faders, mutes, sends and buses change.
    #[test]
    fn the_panel_s_calls_drive_the_mixer() {
        let engine = MockEngine::new().with_kinds([audio_kind("audio_input")]);
        let control = connected(&engine);
        let builtin = |kind: &str| SourceKind::Builtin { kind: kind.into() };
        create_source(&control, "mic", "Mic", builtin("audio_input"));
        create_source(&control, "aux", "aux in", builtin("audio_input"));
        create_source(&control, "pic", "Picture", builtin("image"));
        let scene = Edit::SceneCreate {
            scene_id: SceneId::new("s"),
            name: "S".into(),
            role: None,
        };
        now(edit(&control, scene)).expect("scene");
        let nested = SourceKind::Scene {
            scene_id: SceneId::new("s"),
        };
        create_source(&control, "nested", "Nested", nested);

        let loaded = now(load(&control)).expect("load");
        let names: Vec<&str> = loaded.channels.iter().map(|c| c.name.as_str()).collect();
        // Audio sources only, by name ignoring case; the image has no audio.
        assert_eq!(names, ["aux in", "Mic", "Nested"]);
        let mic = SourceId::new("mic");
        let nested = loaded
            .channel(&SourceId::new("nested"))
            .expect("nested scene");
        assert!(nested.is_scene);
        let channel = loaded.channel(&mic).expect("mic");
        assert!(!channel.is_scene);
        assert_eq!(channel.audio.follow, AudioFollow::Always);
        assert_eq!(loaded.buses.len(), 1);
        assert!(loaded.buses[0].bus_id.is_master());
        // A new channel source sends to the master at 0 dB.
        assert_eq!(loaded.send_gain(&mic, &BusId::master()), Some(0.0));

        let fader = Edit::SourceSetAudio {
            source_id: mic.clone(),
            enabled: None,
            follow: None,
            gain_db: Some(db(-6.0)),
            muted: Some(true),
        };
        now(edit(&control, fader)).expect("source fader");
        let bus_id = next_bus_id(loaded.buses.iter().map(|b| &b.bus_id));
        assert_eq!(bus_id.as_str(), "bus-1");
        let create = Edit::BusCreate {
            bus_id: bus_id.clone(),
            name: "Stream".into(),
        };
        now(edit(&control, create)).expect("bus");
        let send = Edit::SendSet {
            source_id: mic.clone(),
            bus_id: bus_id.clone(),
            gain_db: db(-3.0),
        };
        now(edit(&control, send)).expect("send");
        let master = Edit::BusSetFader {
            bus_id: BusId::master(),
            gain_db: Some(db(2.5)),
            muted: None,
        };
        now(edit(&control, master)).expect("master fader");

        let loaded = now(load(&control)).expect("load");
        let channel = loaded.channel(&mic).expect("mic");
        assert_eq!(channel.audio.gain_db.get(), -6.0);
        assert!(channel.audio.muted);
        assert_eq!(loaded.send_gain(&mic, &bus_id), Some(-3.0));
        // The master comes first, then the others by ID.
        assert!(loaded.buses[0].bus_id.is_master());
        assert_eq!(loaded.buses[0].gain_db.get(), 2.5);
        assert_eq!(loaded.buses[1].bus_id, bus_id);
        assert_eq!(
            next_bus_id(loaded.buses.iter().map(|b| &b.bus_id)).as_str(),
            "bus-2"
        );

        // Deleting the bus takes its sends with it.
        now(edit(
            &control,
            Edit::BusDelete {
                bus_id: bus_id.clone(),
            },
        ))
        .expect("delete");
        let loaded = now(load(&control)).expect("load");
        assert_eq!(loaded.buses.len(), 1);
        assert_eq!(loaded.send_gain(&mic, &bus_id), None);

        // A nested scene has no fader: the engine refuses one.
        let refused = Edit::SourceSetAudio {
            source_id: SourceId::new("nested"),
            enabled: None,
            follow: None,
            gain_db: Some(db(-6.0)),
            muted: None,
        };
        assert!(now(edit(&control, refused)).is_err());
    }

    /// The mock engine, answering `delete.plan` with `plan` at its current revision (the
    /// mock doesn't plan; `jstream-graph`'s tests check the engine's plan) and keeping
    /// every call it is sent.
    struct Planning<C> {
        inner: C,
        plan: Vec<Edit>,
        calls: Mutex<Vec<Call>>,
    }

    impl<C: Control> Control for Planning<C> {
        fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
            self.inner.hello(hello)
        }

        fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
            self.calls.lock().expect("calls").push(call.clone());
            let Call::Query {
                query: Query::DeletePlan { .. },
            } = call
            else {
                return self.inner.call(call);
            };
            Box::pin(async move {
                let state = Call::Query {
                    query: Query::StateGet,
                };
                let mut response = self.inner.call(state).await?;
                let CallResult::Query {
                    result: QueryResult::StateGet { rev, .. },
                } = response.result
                else {
                    panic!("state.get answered with something else");
                };
                response.result = CallResult::Query {
                    result: QueryResult::DeletePlan {
                        rev,
                        edits: self.plan.clone(),
                        hotkeys: Vec::new(),
                    },
                };
                Ok(response)
            })
        }

        fn events(&self) -> EventStream {
            self.inner.events()
        }
    }

    /// #461: a bus a waveform draws is deleted through `delete.plan`, not a plain
    /// `bus.delete` the engine refuses: the strip asks with what the plan clears, then
    /// sends the plan expecting its revision.
    #[test]
    fn a_drawn_bus_is_deleted_through_its_plan() {
        let engine = MockEngine::new();
        let vod = BusId::new("vod");
        let draw = |bus: serde_json::Value| Edit::SourceSetSettings {
            source_id: SourceId::new("wave"),
            patch: [("audio", bus)].into_iter().collect(),
        };
        let delete = Edit::BusDelete {
            bus_id: vod.clone(),
        };
        let control = Planning {
            inner: connected(&engine),
            plan: vec![draw(serde_json::Value::Null), delete.clone()],
            calls: Mutex::new(Vec::new()),
        };
        now(edit(
            &control,
            Edit::BusCreate {
                bus_id: vod.clone(),
                name: "VOD".into(),
            },
        ))
        .expect("bus");
        let waveform = Edit::SourceCreate {
            source_id: SourceId::new("wave"),
            name: "Waveform".into(),
            kind: SourceKind::Builtin {
                kind: "waveform".into(),
            },
            settings: Settings::new(),
            duplicated_from: None,
        };
        now(edit(&control, waveform)).expect("waveform");
        now(edit(
            &control,
            draw(serde_json::json!({"type": "bus", "bus_id": "vod"})),
        ))
        .expect("draws vod");

        // What the strip used to send: the engine refuses it.
        assert!(now(edit(&control, delete.clone())).is_err());

        let plan = now(plan_bus_delete(&control, vod.clone(), "VOD")).expect("plan");
        let asked = control.calls.lock().expect("calls").last().cloned();
        assert_eq!(
            asked,
            Some(Call::Query {
                query: Query::DeletePlan {
                    target: DeleteTarget::Bus {
                        bus_id: vod.clone()
                    },
                },
            })
        );
        assert_eq!(plan.question, "Delete bus “VOD”?");
        assert_eq!(
            plan.detail.as_deref(),
            Some("This also clears 1 setting that refers to it.")
        );
        let rev = now(load(&control)).expect("load").rev;
        assert_eq!(plan.rev, rev);
        assert_eq!(plan.edits, control.plan);

        // A plan made before another change is refused, and deletes nothing.
        let renamed = Edit::BusRename {
            bus_id: vod.clone(),
            name: "Clips".into(),
        };
        now(edit(&control, renamed)).expect("rename");
        assert!(now(apply_plan(&control, plan)).is_err());
        assert!(now(load(&control)).expect("load").bus(&vod).is_some());

        let plan = now(plan_bus_delete(&control, vod.clone(), "Clips")).expect("plan");
        let rev = plan.rev;
        now(apply_plan(&control, plan)).expect("the plan applies");
        let sent = control.calls.lock().expect("calls").last().cloned();
        assert_eq!(
            sent,
            Some(Call::Batch(
                Batch {
                    edits: control.plan.clone(),
                    expected_rev: None,
                    idempotency_key: None,
                    ends_gesture: None,
                }
                .expecting(rev)
            ))
        );
        assert!(now(load(&control)).expect("load").bus(&vod).is_none());
    }

    #[test]
    fn the_snapshot_patches_like_the_engine() {
        let engine = MockEngine::new().with_kinds([audio_kind("audio_input")]);
        let control = connected(&engine);
        let builtin = SourceKind::Builtin {
            kind: "audio_input".into(),
        };
        create_source(&control, "mic", "Mic", builtin);
        let mic = SourceId::new("mic");
        let mut snapshot = now(load(&control)).expect("load");
        snapshot.patch_channel(&mic, |a| a.muted = true);
        snapshot.patch_bus(&BusId::master(), |b| b.gain_db = db(-1.0));
        snapshot.patch_send(&mic, &BusId::master(), None);
        assert!(snapshot.channel(&mic).is_some_and(|c| c.audio.muted));
        assert_eq!(snapshot.buses[0].gain_db.get(), -1.0);
        assert_eq!(snapshot.send_gain(&mic, &BusId::master()), None);
        snapshot.patch_send(&mic, &BusId::master(), Some(db(-2.0)));
        assert_eq!(snapshot.send_gain(&mic, &BusId::master()), Some(-2.0));
    }

    #[test]
    fn the_fader_taper_runs_the_whole_range() {
        assert_eq!(position_to_db(0), -100.0);
        assert_eq!(position_to_db(FADER_STEPS), 24.0);
        assert_eq!(position_to_db(750), 0.0);
        assert_eq!(position_to_db(u16::MAX), 24.0);
        assert_eq!(db_to_position(0.0), 750);
        assert_eq!(db_to_position(-100.0), 0);
        assert_eq!(db_to_position(24.0), FADER_STEPS);
        assert_eq!(db_to_position(-500.0), 0);
        assert_eq!(db_to_position(99.0), FADER_STEPS);
        // Near unity snaps to it.
        assert_eq!(position_to_db(751), 0.0);
        // It rises all the way.
        let mut last = f32::NEG_INFINITY;
        for position in 0..=FADER_STEPS {
            let db = position_to_db(position);
            assert!(db >= last, "{position}: {db} < {last}");
            last = db;
        }
        // A gain goes to a position and back within the fader's resolution.
        for tenth in -1000_i16..=240 {
            let gain = f32::from(tenth) / 10.0;
            let back = position_to_db(db_to_position(gain));
            assert!((back - gain).abs() <= 0.35, "{gain} came back as {back}");
        }
    }

    #[test]
    fn gains_are_checked_and_formatted() {
        assert_eq!(checked_gain(3.0).map(Finite32::get), Ok(3.0));
        assert!(checked_gain(24.5).is_err());
        assert!(checked_gain(-101.0).is_err());
        assert!(checked_gain(f32::NAN).is_err());
        assert_eq!(format_gain(3.0), "+3.0 dB");
        assert_eq!(format_gain(0.0), "0.0 dB");
        assert_eq!(format_gain(-12.5), "-12.5 dB");
    }

    #[test]
    fn the_master_card_reads_loudness_and_true_peak() {
        // The target sits where the prototype draws its marker.
        let target = loudness_fraction(LOUDNESS_TARGET_LUFS);
        assert!((target - 0.7).abs() < 0.01, "{target}");
        assert_eq!(loudness_fraction(-80.0), 0.0);
        assert_eq!(loudness_fraction(10.0), 1.0);
        assert_eq!(true_peak_dbtp(None), None);
        assert_eq!(true_peak_dbtp(Some([0.0, 0.0])), None);
        let half = true_peak_dbtp(Some([0.1, 0.5])).expect("louder channel");
        assert!((half + 6.02).abs() < 0.01, "{half}");
        assert_eq!(true_peak_note(Some(-1.0)), ("safe", false));
        assert_eq!(
            true_peak_note(Some(0.4)),
            ("over 0 dBTP: add a true-peak limiter", true)
        );
        assert_eq!(true_peak_note(None), ("", false));
        assert_eq!(channel_count(1), "1 channel");
        assert_eq!(channel_count(0), "0 channels");
        assert_eq!(channel_count(3), "3 channels");
    }

    #[test]
    fn vertical_strips_follow_the_dock_until_flipped() {
        assert!(meters_vertical(None, true));
        assert!(!meters_vertical(None, false));
        assert!(!meters_vertical(Some(false), true));
        assert!(meters_vertical(Some(true), false));
        assert_eq!(short_name("Mic · ES-9 Line"), "Mic");
        assert_eq!(short_name("Desktop audio"), "Desktop audio");
    }

    #[test]
    fn meters_fill_from_the_floor_to_full_scale() {
        assert_eq!(meter_fraction(0.0), 0.0);
        assert_eq!(meter_fraction(-0.5), 0.0);
        assert_eq!(meter_fraction(0.001), 0.0);
        assert_eq!(meter_fraction(1.0), 1.0);
        assert_eq!(meter_fraction(2.0), 1.0);
        let half = meter_fraction(0.5);
        assert!((half - 0.9).abs() < 0.001, "{half}");
    }

    #[test]
    fn a_peak_is_held_then_falls() {
        let mut hold = Hold::default();
        hold.observe(0.8);
        assert_eq!(hold.level, 0.8);
        for _ in 0..HOLD_TICKS {
            hold.observe(0.1);
            assert_eq!(hold.level, 0.8);
        }
        hold.observe(0.1);
        assert!(hold.level < 0.8 && hold.level >= 0.1);
        for _ in 0..50 {
            hold.observe(0.1);
        }
        assert_eq!(hold.level, 0.1);
        hold.observe(0.9);
        assert_eq!((hold.level, hold.ticks), (0.9, HOLD_TICKS));
    }

    #[test]
    fn the_meter_bank_follows_the_ticks() {
        let levels = |l: f32, r: f32| [db(l), db(r)];
        let mic = SourceId::new("mic");
        let tick_sources = vec![SourceMeter {
            source_id: mic.clone(),
            peak: levels(1.0, 0.5),
            rms: levels(0.25, 0.125),
        }];
        let tick_buses = vec![
            BusMeter {
                bus_id: BusId::master(),
                peak: levels(0.5, 0.5),
                rms: levels(0.1, 0.1),
                true_peak: Some(levels(0.5, 0.25)),
                loudness: Some(Loudness {
                    momentary: Some(db(-23.04)),
                    short_term: None,
                    integrated: None,
                }),
            },
            BusMeter {
                bus_id: BusId::new("bus-1"),
                peak: levels(0.0, 0.0),
                rms: levels(0.0, 0.0),
                true_peak: None,
                loudness: None,
            },
        ];
        let mut bank = MeterBank::default();
        bank.observe(&tick_sources, &tick_buses);
        let reading = bank.sources.get(&mic).copied().expect("mic reading");
        assert_eq!(reading.peak, [1.0, 0.5]);
        assert_eq!(reading.rms, [0.25, 0.125]);
        assert!(reading.clipped());
        assert_eq!(bank.buses.len(), 2);
        assert_eq!(format_true_peak(bank.master.true_peak), "\u{2212}6.0 dBTP");
        let loudness = bank.master.loudness.expect("loudness");
        assert_eq!(format_lufs(loudness.momentary), "\u{2212}23.0");
        assert_eq!(format_lufs(loudness.integrated), "–");

        // A quieter tick keeps a source's hold.
        let quieter = vec![SourceMeter {
            source_id: mic.clone(),
            peak: levels(0.25, 0.25),
            rms: levels(0.1, 0.1),
        }];
        bank.observe(&quieter, &tick_buses);
        let reading = bank.sources.get(&mic).copied().expect("mic reading");
        assert_eq!(reading.peak, [0.25, 0.25]);
        assert_eq!(reading.hold[0].level, 1.0);
        assert_eq!(reading.hold[0].ticks, HOLD_TICKS - 1);

        // A tick without the source drops its reading; when it comes back, it starts
        // afresh.
        bank.observe(&[], &tick_buses);
        assert!(bank.sources.is_empty());
        bank.observe(&tick_sources, &tick_buses[1..]);
        let reading = bank.sources.get(&mic).copied().expect("mic reading");
        assert_eq!(reading.hold[0].ticks, HOLD_TICKS);
        assert_eq!(bank.master.true_peak, None);
        bank.clear();
        assert_eq!(bank, MeterBank::default());
        assert_eq!(format_true_peak(None), "– dBTP");
    }
}
