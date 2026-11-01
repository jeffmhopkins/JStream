//! The Stream stats panel (ST-01, spec-panels.md 6): one output's live stats from
//! `stats.tick`'s `outputs` (protocol 0.59). The status line has the LIVE or READY pill,
//! the output and its uptime; under it the bitrate and the dropped frames, the health the
//! engine works out, a two-minute bitrate chart kept here, and the output's details.
//!
//! The panel shows the output the person picks, or else the first running stream, the
//! first running output, then the first stream. What only YouTube knows (watching,
//! likes, chat, YouTube's own health) needs a sign-in this build doesn't have, so those
//! tiles aren't shown.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#live-panels

use crate::app::SessionContext;
use crate::panels::outputs::{OutputRow, describe_kind, is_running, load};
use futures_util::StreamExt;
use jstream_control::protocol::{
    AudioCodec, EncoderProfile, EncoderProfileId, EncoderProfileInfo, Event, OutputHealth,
    OutputId, OutputKind, OutputState, OutputStatsInfo, RateControl, VideoCodec,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::{BTreeMap, VecDeque};

/// How many samples the chart keeps: two minutes of ticks, about one a second.
pub const CHART_SAMPLES: usize = 120;

/// The chart's drawing box, as the prototype's: 240×64 user units.
const CHART_W: f64 = 240.0;
const CHART_H: f64 = 64.0;

/// What the panel keeps from the ticks: each running output's latest stats and its
/// bitrate over the last [`CHART_SAMPLES`] ticks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Readings {
    /// The latest stats of each output the last tick listed.
    pub latest: BTreeMap<OutputId, OutputStatsInfo>,
    /// Each listed output's `kbps`, oldest first.
    pub history: BTreeMap<OutputId, VecDeque<u32>>,
}

impl Readings {
    /// Takes one tick's `outputs`. An output the tick leaves out has stopped, and its
    /// history goes; one whose uptime went down was started again, and its history
    /// starts over.
    pub fn observe(&mut self, outputs: &[OutputStatsInfo]) {
        let listed: Vec<&OutputId> = outputs.iter().map(|o| &o.output_id).collect();
        self.latest.retain(|id, _| listed.contains(&id));
        self.history.retain(|id, _| listed.contains(&id));
        for output in outputs {
            let restarted = self
                .latest
                .get(&output.output_id)
                .is_some_and(|before| output.uptime_ms < before.uptime_ms);
            let history = self.history.entry(output.output_id.clone()).or_default();
            if restarted {
                history.clear();
            }
            history.push_back(output.kbps);
            while history.len() > CHART_SAMPLES {
                history.pop_front();
            }
            self.latest.insert(output.output_id.clone(), output.clone());
        }
    }
}

/// The output the panel shows: `picked` while it exists; else the first running
/// stream, the first running output, the first stream, then the first output.
pub fn shown_output(rows: &[OutputRow], picked: Option<&OutputId>) -> Option<OutputId> {
    if let Some(picked) = picked
        && rows.iter().any(|row| &row.info.output_id == picked)
    {
        return Some(picked.clone());
    }
    let stream = |row: &&OutputRow| matches!(row.info.kind, OutputKind::Rtmp { .. });
    let running = |row: &&OutputRow| is_running(row.state);
    rows.iter()
        .filter(stream)
        .find(running)
        .or_else(|| rows.iter().find(running))
        .or_else(|| rows.iter().find(stream))
        .or_else(|| rows.first())
        .map(|row| row.info.output_id.clone())
}

/// Whether the pill says LIVE: the output runs and is past starting.
pub fn is_live(state: OutputState) -> bool {
    matches!(state, OutputState::Active | OutputState::Reconnecting)
}

/// An uptime as HH:MM:SS, or `--:--:--` when not live.
pub fn uptime(ms: Option<u64>) -> String {
    let Some(ms) = ms else {
        return "--:--:--".into();
    };
    let secs = ms / 1000;
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// A bitrate in Mbps with two decimals, as the prototype shows it.
pub fn mbps(kbps: u32) -> String {
    format!("{:.2} Mbps", f64::from(kbps) / 1000.0)
}

/// The dropped-frames tile's sub line: the share of this run's video frames lost.
pub fn dropped_share(stats: &OutputStatsInfo) -> String {
    let total = stats.frames.saturating_add(stats.dropped_frames);
    let share = if total == 0 {
        0.0
    } else {
        stats.dropped_frames as f64 * 100.0 / total as f64
    };
    format!("{share:.2}% this run")
}

/// The health card's label, note and style class.
pub fn health_text(health: Option<OutputHealth>) -> (&'static str, &'static str, &'static str) {
    match health {
        None => ("Waiting", "The output isn't running.", "waiting"),
        Some(OutputHealth::Waiting) => (
            "Waiting",
            "The output hasn't sent any video yet.",
            "waiting",
        ),
        Some(OutputHealth::Good) => (
            "Excellent",
            "Nothing dropped in the last 10 seconds.",
            "good",
        ),
        Some(OutputHealth::Fair) => (
            "OK",
            "A few frames or packets were dropped in the last 10 seconds.",
            "fair",
        ),
        Some(OutputHealth::Poor) => (
            "Poor",
            "The connection is down and reconnecting, or 2% or more of the frames were dropped in the last 10 seconds.",
            "poor",
        ),
        Some(_) => (
            "Unknown",
            "The engine reports a health this page doesn't know.",
            "waiting",
        ),
    }
}

/// The profile an output uses: its own, or the engine's default when it names none
/// or one the page doesn't have.
pub fn profile_of<'a>(
    profiles: &'a [EncoderProfileInfo],
    id: Option<&EncoderProfileId>,
) -> &'a EncoderProfile {
    id.and_then(|id| profiles.iter().find(|p| &p.profile_id == id))
        .map_or(&EncoderProfile::DEFAULT, |p| &p.profile)
}

/// The video bitrate an output aims for, in kbps, if its rate control says.
pub fn target_kbps(profile: &EncoderProfile) -> Option<u32> {
    match profile.video.rate_control {
        RateControl::Cbr { kbps } | RateControl::Vbr { kbps, .. } => Some(kbps),
        _ => None,
    }
}

/// The details rows' video line: codec and rate control.
pub fn video_line(profile: &EncoderProfile) -> String {
    let codec = match profile.video.codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::Hevc => "HEVC",
        VideoCodec::Av1 => "AV1",
        _ => "an unknown codec",
    };
    match profile.video.rate_control {
        RateControl::Cbr { kbps } => format!("{codec} · {kbps} kbps CBR"),
        RateControl::Vbr { kbps, max_kbps } => {
            format!("{codec} · {kbps} kbps VBR (peak {max_kbps})")
        }
        _ => codec.to_owned(),
    }
}

/// The details rows' audio line.
pub fn audio_line(profile: &EncoderProfile) -> String {
    let codec = match profile.audio.codec {
        AudioCodec::Aac => "AAC",
        _ => "an unknown codec",
    };
    format!("{codec} · {} kbps", profile.audio.kbps)
}

/// A byte count with a unit: B, kB, MB or GB, decimal.
pub fn bytes(n: u64) -> String {
    const UNITS: [(&str, f64); 3] = [("GB", 1e9), ("MB", 1e6), ("kB", 1e3)];
    let value = n as f64;
    for (unit, size) in UNITS {
        if value >= size {
            return format!("{:.1} {unit}", value / size);
        }
    }
    format!("{n} B")
}

/// The chart's vertical range in kbps. Around a target, as the prototype's 4–7 Mbps
/// around 6 Mbps (two thirds to seven sixths of it), widened to fit the data; with no
/// target, the data's own range.
pub fn chart_range(samples: &VecDeque<u32>, target: Option<u32>) -> (f64, f64) {
    let lowest = samples.iter().copied().min().map_or(0.0, f64::from);
    let highest = samples.iter().copied().max().map_or(0.0, f64::from);
    let (low, high) = match target {
        Some(target) => {
            let target = f64::from(target);
            (
                (target * 2.0 / 3.0).min(lowest),
                (target * 7.0 / 6.0).max(highest),
            )
        }
        None => (lowest, highest),
    };
    if high - low < 1.0 {
        (low, low + 1.0)
    } else {
        (low, high)
    }
}

/// The chart's y for `kbps` in `range`, top down.
fn chart_y(kbps: f64, (low, high): (f64, f64)) -> f64 {
    CHART_H - (kbps - low) / (high - low) * CHART_H
}

/// The chart line's `points`: the newest sample at the right edge, one step a sample
/// over [`CHART_SAMPLES`], so a short history fills from the right as time does.
pub fn chart_points(samples: &VecDeque<u32>, range: (f64, f64)) -> String {
    let step = CHART_W / (CHART_SAMPLES - 1) as f64;
    let start = CHART_SAMPLES - samples.len().min(CHART_SAMPLES);
    samples
        .iter()
        .enumerate()
        .map(|(i, &kbps)| {
            let x = (start + i) as f64 * step;
            format!("{x:.1},{:.1}", chart_y(f64::from(kbps), range))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The Stream stats panel. Reads the open session from [`SessionContext`] context.
#[component]
pub fn StreamStats() -> impl IntoView {
    let Some(session) = use_context::<SessionContext>() else {
        return view! { <p class="placeholder">"Not connected."</p> }.into_any();
    };
    let rows = RwSignal::new(Vec::<OutputRow>::new());
    let profiles = RwSignal::new(Vec::<EncoderProfileInfo>::new());
    let readings = RwSignal::new(Readings::default());
    let picked = RwSignal::new(None::<OutputId>);
    let problem = RwSignal::new(None::<String>);
    let loads = StoredValue::new(0_u64);

    let reload = move || {
        let Some(session) = session.get() else {
            return;
        };
        let Some(mine) = loads.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        spawn_local(async move {
            let loaded = load(&*session.control).await;
            if loads.try_get_value() != Some(mine) {
                return;
            }
            match loaded {
                Ok(snapshot) => {
                    problem.try_set(None);
                    rows.try_set(snapshot.outputs);
                    profiles.try_set(snapshot.encoders);
                }
                Err(error) => {
                    problem.try_set(Some(error));
                }
            }
        });
    };

    if let Some(open) = session.get() {
        let mut events = open.control.events();
        drop(open);
        reload();
        spawn_local(async move {
            while let Some(event) = events.next().await {
                match event {
                    Event::StateChanged { .. } | Event::EventsDropped { .. } => reload(),
                    Event::OutputStateChanged { output_id, state } => {
                        let known = rows.try_update(|rows| {
                            rows.iter_mut()
                                .find(|row| row.info.output_id == output_id)
                                .map(|row| row.state = state)
                                .is_some()
                        });
                        if known == Some(false) {
                            reload();
                        }
                    }
                    Event::StatsTick { outputs, .. } => {
                        readings.try_update(|r| r.observe(&outputs));
                    }
                    _ => {}
                }
            }
        });
    }

    let shown = Memo::new(move |_| {
        let picked = picked.get();
        rows.with(|rows| shown_output(rows, picked.as_ref()))
    });
    let row = Memo::new(move |_| {
        let id = shown.get()?;
        rows.with(|rows| rows.iter().find(|row| row.info.output_id == id).cloned())
    });
    let stats = Memo::new(move |_| {
        let id = shown.get()?;
        readings.with(|r| r.latest.get(&id).cloned())
    });
    let live = move || row.with(|row| row.as_ref().is_some_and(|row| is_live(row.state)));
    let profile = Memo::new(move |_| {
        let id = row.with(|row| {
            row.as_ref()
                .and_then(|row| row.info.encoder_profile.clone())
        });
        profiles.with(|profiles| *profile_of(profiles, id.as_ref()))
    });
    let target = move || profile.with(target_kbps);
    let samples = move || {
        let Some(id) = shown.get() else {
            return VecDeque::new();
        };
        readings.with(|r| r.history.get(&id).cloned().unwrap_or_default())
    };

    let picker = move || {
        let ids: Vec<OutputId> =
            rows.with(|rows| rows.iter().map(|row| row.info.output_id.clone()).collect());
        (ids.len() > 1).then(|| {
            let current = shown.get().map(|id| id.to_string()).unwrap_or_default();
            view! {
                <select
                    class="stats-output"
                    aria-label="Output"
                    prop:value=current
                    on:change=move |event| {
                        picked.set(Some(OutputId::new(event_target_value(&event))));
                    }
                >
                    {ids
                        .into_iter()
                        .map(|id| {
                            let value = id.to_string();
                            view! { <option value=value.clone()>{value.clone()}</option> }
                        })
                        .collect_view()}
                </select>
            }
        })
    };

    let chart = move || {
        let samples = samples();
        if samples.is_empty() || !live() {
            return view! {
                <div class="stats-nodata">"No data yet. Start the output to see its bitrate."</div>
            }
            .into_any();
        }
        let target = target();
        let range = chart_range(&samples, target);
        let points = chart_points(&samples, range);
        let target_line = target.map(|kbps| {
            let y = format!("{:.1}", chart_y(f64::from(kbps), range));
            view! {
                <line
                    x1="0"
                    x2=CHART_W
                    y1=y.clone()
                    y2=y
                    stroke="#3A3F48"
                    stroke-dasharray="3 3"
                    vector-effect="non-scaling-stroke"
                />
            }
        });
        let caption = target.map_or_else(
            || "bitrate".to_owned(),
            |kbps| format!("target {:.1} Mbps · dashed", f64::from(kbps) / 1000.0),
        );
        view! {
            <svg
                class="stats-chart"
                viewBox=format!("0 0 {CHART_W} {CHART_H}")
                preserveAspectRatio="none"
                role="img"
                aria-label="Bitrate over the last two minutes"
            >
                {target_line}
                <line
                    x1="0"
                    x2=CHART_W
                    y1=CHART_H
                    y2=CHART_H
                    stroke="#2E333B"
                    vector-effect="non-scaling-stroke"
                />
                <polyline
                    points=points
                    fill="none"
                    stroke="#4C8DFF"
                    stroke-width="2"
                    vector-effect="non-scaling-stroke"
                />
            </svg>
            <div class="stats-axis">
                <span>"2 min ago"</span>
                <span>{caption}</span>
                <span>"now"</span>
            </div>
        }
        .into_any()
    };

    view! {
        <section class="stream-stats">
            <h2>"Stream stats"</h2>
            {move || problem.get().map(|p| view! { <p class="problem" role="alert">{p}</p> })}
            {move || {
                if row.with(Option::is_none) {
                    return view! {
                        <p class="placeholder">
                            "No outputs yet. Add a stream or a recording in Outputs to see its stats here."
                        </p>
                    }
                        .into_any();
                }
                view! {
                    <div class="stats-status">
                        <span class="stats-pill" class:live=live>
                            <span class="dot"></span>
                            {move || if live() { "LIVE" } else { "READY" }}
                        </span>
                        <span class="stats-title">
                            {move || {
                                row.with(|row| {
                                    row.as_ref().map(|row| describe_kind(&row.info.kind))
                                })
                            }}
                        </span>
                        <span class="stats-uptime mono">
                            {move || uptime(
                                stats.with(|s| s.as_ref().map(|s| s.uptime_ms)).filter(|_| live()),
                            )}
                        </span>
                    </div>
                    {picker}
                    <div class="stats-tiles">
                        <div class="stats-tile">
                            <span class="label">"Bitrate"</span>
                            <span class="value mono">
                                {move || {
                                    stats
                                        .with(|s| s.as_ref().map(|s| mbps(s.kbps)))
                                        .filter(|_| live())
                                        .unwrap_or_else(|| "–".into())
                                }}
                            </span>
                            <span class="sub">
                                {move || {
                                    if !live() {
                                        return "not live".to_owned();
                                    }
                                    target()
                                        .map_or_else(
                                            || "no target".to_owned(),
                                            |kbps| format!("target {}", mbps(kbps)),
                                        )
                                }}
                            </span>
                        </div>
                        <div class="stats-tile">
                            <span class="label">"Dropped frames"</span>
                            <span class="value mono">
                                {move || {
                                    stats
                                        .with(|s| s.as_ref().map(|s| s.dropped_frames.to_string()))
                                        .filter(|_| live())
                                        .unwrap_or_else(|| "–".into())
                                }}
                            </span>
                            <span class="sub">
                                {move || {
                                    stats
                                        .with(|s| s.as_ref().map(dropped_share))
                                        .filter(|_| live())
                                        .unwrap_or_else(|| "not live".into())
                                }}
                            </span>
                        </div>
                    </div>
                    {move || {
                        let health = stats
                            .with(|s| s.as_ref().map(|s| s.health))
                            .filter(|_| live());
                        let (label, note, class) = health_text(health);
                        view! {
                            <div class=format!("stats-health {class}") role="status">
                                <span class="dot"></span>
                                <div>
                                    <div class="title">{format!("Stream health: {label}")}</div>
                                    <div class="note">{note}</div>
                                </div>
                            </div>
                        }
                    }}
                    <div class="stats-chart-head">
                        <span>"BITRATE · LAST 2 MIN"</span>
                        <span class="mono">
                            {move || {
                                stats
                                    .with(|s| s.as_ref().map(|s| mbps(s.kbps)))
                                    .filter(|_| live())
                                    .unwrap_or_default()
                            }}
                        </span>
                    </div>
                    {chart}
                    <dl class="stats-details">
                        <dt>"Video"</dt>
                        <dd class="mono">{move || profile.with(video_line)}</dd>
                        <dt>"Audio"</dt>
                        <dd class="mono">{move || profile.with(audio_line)}</dd>
                        <dt>"Sent"</dt>
                        <dd class="mono">
                            {move || {
                                stats
                                    .with(|s| {
                                        s.as_ref().map(|s| format!("{} · {} frames", bytes(s.bytes), s.frames))
                                    })
                                    .unwrap_or_else(|| "–".into())
                            }}
                        </dd>
                        <dt>"Other drops"</dt>
                        <dd class="mono">
                            {move || {
                                stats
                                    .with(|s| s.as_ref().map(|s| s.dropped_packets.to_string()))
                                    .unwrap_or_else(|| "–".into())
                            }}
                        </dd>
                    </dl>
                }
                    .into_any()
            }}
        </section>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{
        Readings, audio_line, bytes, chart_points, chart_range, dropped_share, health_text, mbps,
        shown_output, uptime, video_line,
    };
    use crate::panels::outputs::OutputRow;
    use jstream_control::protocol::{
        EncoderProfile, FilterChains, OutputHealth, OutputId, OutputInfo, OutputKind, OutputState,
        OutputStatsInfo, RateControl, RecordContainer, RtmpTarget, VideoCodec,
    };
    use std::collections::VecDeque;

    fn stats(id: &str, uptime_ms: u64, kbps: u32) -> OutputStatsInfo {
        OutputStatsInfo {
            output_id: OutputId::new(id),
            uptime_ms,
            bytes: 0,
            frames: 0,
            kbps,
            dropped_frames: 0,
            dropped_packets: 0,
            health: OutputHealth::Good,
        }
    }

    fn row(id: &str, stream: bool, state: OutputState) -> OutputRow {
        let kind = if stream {
            OutputKind::Rtmp {
                target: RtmpTarget::Generic {
                    url: "rtmp://127.0.0.1/live".into(),
                },
            }
        } else {
            OutputKind::Record {
                path: "/tmp/a.mp4".into(),
                container: RecordContainer::Mp4,
            }
        };
        OutputRow {
            info: OutputInfo {
                output_id: OutputId::new(id),
                kind,
                encoder_profile: None,
                filters: FilterChains::default(),
                video_mix: None,
            },
            state,
            key_set: true,
            cleartext: false,
        }
    }

    #[test]
    fn readings_keep_two_minutes_and_forget_stopped_outputs() {
        let mut readings = Readings::default();
        for s in 0..130 {
            readings.observe(&[stats("a", s * 1000, u32::try_from(s).unwrap_or(0))]);
        }
        let a = &readings.history[&OutputId::new("a")];
        assert_eq!(a.len(), 120);
        assert_eq!(a.front(), Some(&10));
        assert_eq!(a.back(), Some(&129));

        // Started again: its uptime went down, so its history starts over.
        readings.observe(&[stats("a", 500, 7), stats("b", 1000, 9)]);
        assert_eq!(readings.history[&OutputId::new("a")], VecDeque::from([7]));
        assert_eq!(readings.latest.len(), 2);

        // Stopped: the tick leaves it out.
        readings.observe(&[stats("b", 2000, 9)]);
        assert!(!readings.latest.contains_key(&OutputId::new("a")));
        assert!(!readings.history.contains_key(&OutputId::new("a")));
    }

    #[test]
    fn the_panel_shows_the_pick_then_a_running_stream() {
        let rows = vec![
            row("rec", false, OutputState::Active),
            row("s1", true, OutputState::Stopped),
            row("s2", true, OutputState::Active),
        ];
        assert_eq!(shown_output(&rows, None), Some(OutputId::new("s2")));
        assert_eq!(
            shown_output(&rows, Some(&OutputId::new("rec"))),
            Some(OutputId::new("rec"))
        );
        // A pick that went away falls back.
        assert_eq!(
            shown_output(&rows, Some(&OutputId::new("gone"))),
            Some(OutputId::new("s2"))
        );
        let idle = vec![
            row("rec", false, OutputState::Active),
            row("s1", true, OutputState::Stopped),
        ];
        assert_eq!(shown_output(&idle, None), Some(OutputId::new("rec")));
        let stopped = vec![
            row("rec", false, OutputState::Stopped),
            row("s1", true, OutputState::Stopped),
        ];
        assert_eq!(shown_output(&stopped, None), Some(OutputId::new("s1")));
        assert_eq!(shown_output(&[], None), None);
    }

    #[test]
    fn the_figures_read_as_the_prototype_shows_them() {
        assert_eq!(uptime(None), "--:--:--");
        assert_eq!(uptime(Some(3_723_456)), "01:02:03");
        assert_eq!(mbps(6_012), "6.01 Mbps");
        let mut s = stats("a", 0, 0);
        assert_eq!(dropped_share(&s), "0.00% this run");
        s.frames = 990;
        s.dropped_frames = 10;
        assert_eq!(dropped_share(&s), "1.00% this run");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(45_750_000), "45.8 MB");
        assert_eq!(health_text(Some(OutputHealth::Good)).0, "Excellent");
        assert_eq!(health_text(Some(OutputHealth::Fair)).0, "OK");
        assert_eq!(health_text(None).0, "Waiting");

        let mut profile = EncoderProfile::DEFAULT;
        assert_eq!(video_line(&profile), "H.264 · 2500 kbps CBR");
        assert_eq!(audio_line(&profile), "AAC · 160 kbps");
        profile.video.codec = VideoCodec::Av1;
        profile.video.rate_control = RateControl::Vbr {
            kbps: 8000,
            max_kbps: 12000,
        };
        assert_eq!(video_line(&profile), "AV1 · 8000 kbps VBR (peak 12000)");
    }

    #[test]
    fn the_chart_spans_the_target_and_fills_from_the_right() {
        let samples = VecDeque::from([6000, 6000]);
        assert_eq!(chart_range(&samples, Some(6000)), (4000.0, 7000.0));
        // Data outside the band widens it.
        let wide = VecDeque::from([3000, 8000]);
        assert_eq!(chart_range(&wide, Some(6000)), (3000.0, 8000.0));
        // Flat data with no target still has a range.
        assert_eq!(chart_range(&samples, None), (6000.0, 6001.0));
        // Two samples: the last two steps of the width, at 6000 of 4000..7000.
        assert_eq!(
            chart_points(&samples, (4000.0, 7000.0)),
            "238.0,21.3 240.0,21.3"
        );
    }
}
