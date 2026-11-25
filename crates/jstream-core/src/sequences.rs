//! What plays and stops sequences ([ADR-0184](../../../docs/adr/0184-sequences-lanes-and-triggers.md)):
//! the timelines waiting on a delay, and the triggers in each timeline's `starts` and
//! `stops`, checked on the `jstream-values` thread's tick.
//!
//! A trigger fires on a change only: the first tick after the engine starts, or after
//! a collection opens, sees the state as it is and fires nothing. A channel trigger
//! fires at most once every [`MIN_REFIRE`], and crosses back only past a dead band of
//! [`DEAD_BAND`], so a noisy value sitting on its level doesn't fire again and again.
//! The lanes themselves are the channel driver's (`channels.rs`).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use jstream_graph::Graph;
use jstream_protocol::{
    ChannelEdge, KeyframeEdge, SceneEdge, SceneId, TimelineEdge, TimelineId, TimelineTrigger,
    WaitingTimeline,
};

/// How far back past its level a channel must go before the same edge fires again.
pub(crate) const DEAD_BAND: f32 = 0.02;

/// The shortest time between two firings of one channel trigger.
pub(crate) const MIN_REFIRE: Duration = Duration::from_millis(50);

/// A trigger by its timeline, whether it is a stop, and its place in the list.
type TriggerKey = (TimelineId, bool, usize);

/// What a fired trigger asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Order {
    /// Play the timeline from its start after `delay_ms`.
    Play {
        timeline_id: TimelineId,
        delay_ms: u32,
    },
    /// Stop it, and cancel any wait.
    Stop { timeline_id: TimelineId },
}

/// A timeline waiting to play.
#[derive(Clone, Debug)]
struct Wait {
    scene_id: SceneId,
    due: Instant,
}

/// What the last check saw.
#[derive(Clone, Debug, Default)]
struct Seen {
    program: Option<SceneId>,
    /// Each scene drawn through a keyframe, and that keyframe.
    keyframes: BTreeMap<SceneId, SceneId>,
    /// Each channel's on switch.
    on: BTreeMap<String, bool>,
}

/// The waits and the triggers' state.
#[derive(Debug, Default)]
pub(crate) struct Sequences {
    waits: BTreeMap<TimelineId, Wait>,
    /// The timelines that started, ended or looped since the last check.
    events: Vec<(TimelineId, TimelineEdge)>,
    /// `None` until the first check, which only looks.
    seen: Option<Seen>,
    /// Each channel trigger's side of its level, whether an edge waits for `MIN_REFIRE`,
    /// and when it last fired, by its timeline, whether it is a stop, and its place in
    /// the list.
    above: BTreeMap<TriggerKey, (bool, bool, Option<Instant>)>,
}

impl Sequences {
    /// Plays `id` after `delay_ms`: a wait already running starts again.
    pub(crate) fn wait(&mut self, id: &TimelineId, scene_id: SceneId, delay_ms: u32, now: Instant) {
        self.waits.insert(
            id.clone(),
            Wait {
                scene_id,
                due: now + Duration::from_millis(u64::from(delay_ms)),
            },
        );
    }

    /// Cancels `id`'s wait, if it has one.
    pub(crate) fn cancel(&mut self, id: &TimelineId) {
        self.waits.remove(id);
    }

    /// Whether anything waits.
    pub(crate) fn is_waiting(&self) -> bool {
        !self.waits.is_empty()
    }

    /// The timelines whose wait is over, which it forgets.
    pub(crate) fn due(&mut self, now: Instant) -> Vec<TimelineId> {
        let due: Vec<TimelineId> = self
            .waits
            .iter()
            .filter(|(_, wait)| wait.due <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &due {
            self.waits.remove(id);
        }
        due
    }

    /// What waits, as `studio.get` reports it.
    pub(crate) fn waiting(&self, now: Instant) -> Vec<WaitingTimeline> {
        self.waits
            .iter()
            .map(|(id, wait)| WaitingTimeline {
                timeline_id: id.clone(),
                scene_id: wait.scene_id.clone(),
                due_in_ms: u32::try_from(wait.due.saturating_duration_since(now).as_millis())
                    .unwrap_or(u32::MAX),
            })
            .collect()
    }

    /// Notes that `id` started, ended or looped, for the next check.
    pub(crate) fn timeline_event(&mut self, id: &TimelineId, edge: TimelineEdge) {
        self.events.push((id.clone(), edge));
    }

    /// Forgets what it saw, the waits and the events: a collection opened, so the next
    /// check only looks.
    pub(crate) fn restart(&mut self) {
        *self = Sequences::default();
    }

    /// Whether `graph` holds any trigger, so the values thread must keep checking.
    pub(crate) fn has_triggers(graph: &Graph) -> bool {
        graph
            .state()
            .scenes()
            .iter()
            .flat_map(|scene| scene.timelines().values())
            .any(|t| !t.starts.is_empty() || !t.stops.is_empty())
    }

    /// Checks every trigger of `graph`'s timelines against what changed since the last
    /// check: the program scene, the keyframes scenes draw through, the channels'
    /// values and switches (`level` gives a channel's value and whether it is on), and
    /// the timeline events noted. Returns what they ask for, in the order of the
    /// timelines and their lists.
    pub(crate) fn check(
        &mut self,
        graph: &Graph,
        program: Option<&SceneId>,
        level: impl Fn(&str) -> Option<(f32, bool)>,
        now: Instant,
    ) -> Vec<Order> {
        let state = graph.state();
        let mut keyframes = graph.placed_keyframes().clone();
        if let Some(program) = program
            && let Some(base) = state.scene(program).and_then(|scene| scene.variant_of())
        {
            keyframes.insert(base.clone(), program.clone());
        }
        let mut on = BTreeMap::new();
        for channel in graph.channels() {
            if let Some((_, switched)) = level(&channel.channel_id) {
                on.insert(channel.channel_id.clone(), switched);
            }
        }
        let now_seen = Seen {
            program: program.cloned(),
            keyframes,
            on,
        };
        let events = std::mem::take(&mut self.events);
        let first = self.seen.is_none();
        let before = self.seen.replace(now_seen.clone()).unwrap_or_default();
        let mut orders = Vec::new();
        let mut kept = BTreeMap::new();
        for scene in state.scenes() {
            for timeline in scene.timelines().values() {
                for (stop, list) in [(false, &timeline.starts), (true, &timeline.stops)] {
                    for (n, trigger) in list.iter().enumerate() {
                        let key = (timeline.timeline_id.clone(), stop, n);
                        let fired = match trigger {
                            TimelineTrigger::Channel {
                                channel_id,
                                edge,
                                level: at,
                                ..
                            } => {
                                let Some((value, switched)) = level(channel_id) else {
                                    continue;
                                };
                                let at = at.map_or(0.5, |l| l.get());
                                let (was_above, pending, last) = self
                                    .above
                                    .get(&key)
                                    .copied()
                                    .unwrap_or((value >= at, false, None));
                                let above = if was_above {
                                    value >= at - DEAD_BAND
                                } else {
                                    value >= at
                                };
                                let ready =
                                    last.is_none_or(|l| now.duration_since(l) >= MIN_REFIRE);
                                let was_on = before.on.get(channel_id).copied();
                                let (crossed, holds) = match edge {
                                    ChannelEdge::Rises => (!was_above && above, above),
                                    ChannelEdge::Falls => (was_above && !above, !above),
                                    ChannelEdge::On => {
                                        (was_on == Some(false) && switched, switched)
                                    }
                                    ChannelEdge::Off => {
                                        (was_on == Some(true) && !switched, !switched)
                                    }
                                };
                                // An edge MIN_REFIRE holds back is deferred, not lost: it
                                // fires once the trigger is ready, if the channel is still
                                // on that side.
                                let wants = !first && (crossed || pending) && holds;
                                let fires = wants && ready;
                                kept.insert(
                                    key,
                                    (above, wants && !ready, if fires { Some(now) } else { last }),
                                );
                                fires
                            }
                            TimelineTrigger::Scene { scene_id, edge, .. } => {
                                let changed = before.program != now_seen.program;
                                !first
                                    && changed
                                    && match edge {
                                        SceneEdge::Shown => {
                                            now_seen.program.as_ref() == Some(scene_id)
                                        }
                                        SceneEdge::Hidden => {
                                            before.program.as_ref() == Some(scene_id)
                                        }
                                    }
                            }
                            TimelineTrigger::Keyframe {
                                scene_id,
                                keyframe_id,
                                edge,
                                ..
                            } => {
                                let was = before.keyframes.get(scene_id);
                                let is = now_seen.keyframes.get(scene_id);
                                let matches = |k: Option<&SceneId>| {
                                    k.is_some_and(|k| {
                                        keyframe_id.as_ref().is_none_or(|want| want == k)
                                    })
                                };
                                !first
                                    && was != is
                                    && match edge {
                                        KeyframeEdge::Loads => matches(is),
                                        KeyframeEdge::Leaves => matches(was),
                                    }
                            }
                            TimelineTrigger::Timeline {
                                timeline_id, edge, ..
                            } => events.iter().any(|(id, e)| id == timeline_id && e == edge),
                            TimelineTrigger::Unknown => false,
                        };
                        if fired {
                            let timeline_id = timeline.timeline_id.clone();
                            orders.push(if stop {
                                Order::Stop { timeline_id }
                            } else {
                                Order::Play {
                                    timeline_id,
                                    delay_ms: trigger.delay_ms(),
                                }
                            });
                        }
                    }
                }
            }
        }
        self.above = kept;
        orders
    }
}

#[cfg(test)]
mod tests {
    use super::{Order, Sequences};
    use jstream_graph::Graph;
    use jstream_protocol::{
        ChannelEdge, ChannelInfo, Edit, SceneEdge, SceneId, TimelineEdge, TimelineId, TimelineInfo,
        TimelineLoop, TimelineTrigger,
    };
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    fn graph(starts: Vec<TimelineTrigger>, stops: Vec<TimelineTrigger>) -> Graph {
        let mut graph = Graph::new();
        let edits = vec![
            Edit::SceneCreate {
                scene_id: SceneId::new("s"),
                name: "S".into(),
                role: None,
            },
            Edit::ChannelSet {
                channel: ChannelInfo::new("power"),
            },
            Edit::TimelineSet {
                scene_id: SceneId::new("s"),
                timeline: TimelineInfo {
                    folder: None,
                    timeline_id: TimelineId::new("up"),
                    name: String::new(),
                    duration_ms: 1000,
                    looping: TimelineLoop::Once,
                    cues: Vec::new(),
                    labels: Vec::new(),
                    lanes: Vec::new(),
                    starts,
                    stops,
                },
            },
        ];
        let applied = graph.apply(&edits, None);
        assert!(applied.is_ok(), "{applied:?}");
        graph
    }

    fn play() -> Order {
        Order::Play {
            timeline_id: TimelineId::new("up"),
            delay_ms: 0,
        }
    }

    /// A channel rising past its level plays, falling stops, nothing fires on the first
    /// look, and a value hovering on the level fires once.
    #[test]
    fn a_channel_crossing_its_level_plays_and_stops() {
        let g = graph(
            vec![TimelineTrigger::Channel {
                channel_id: "power".into(),
                edge: ChannelEdge::Rises,
                level: None,
                delay_ms: 0,
            }],
            vec![TimelineTrigger::Channel {
                channel_id: "power".into(),
                edge: ChannelEdge::Falls,
                level: None,
                delay_ms: 0,
            }],
        );
        let value = Cell::new(1.0_f32);
        let mut seq = Sequences::default();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let level = |_: &str| Some((value.get(), true));
        assert!(
            seq.check(&g, None, level, at(0)).is_empty(),
            "the first look"
        );
        value.set(0.2);
        assert_eq!(
            seq.check(&g, None, level, at(100)),
            [Order::Stop {
                timeline_id: TimelineId::new("up")
            }]
        );
        value.set(0.6);
        assert_eq!(seq.check(&g, None, level, at(200)), [play()]);
        value.set(0.49);
        assert!(
            seq.check(&g, None, level, at(300)).is_empty(),
            "within the dead band"
        );
        value.set(0.51);
        assert!(seq.check(&g, None, level, at(400)).is_empty());
    }

    /// Review N1 on #1640: rise, fall and rise again inside `MIN_REFIRE` plays, stops,
    /// and plays again once the trigger is ready, rather than losing the second rise.
    #[test]
    fn an_edge_held_back_by_min_refire_fires_later() {
        let g = graph(
            vec![TimelineTrigger::Channel {
                channel_id: "power".into(),
                edge: ChannelEdge::Rises,
                level: None,
                delay_ms: 0,
            }],
            vec![TimelineTrigger::Channel {
                channel_id: "power".into(),
                edge: ChannelEdge::Falls,
                level: None,
                delay_ms: 0,
            }],
        );
        let value = Cell::new(0.0_f32);
        let mut seq = Sequences::default();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let level = |_: &str| Some((value.get(), true));
        assert!(seq.check(&g, None, level, at(0)).is_empty());
        value.set(1.0);
        assert_eq!(seq.check(&g, None, level, at(100)), [play()]);
        value.set(0.0);
        assert_eq!(
            seq.check(&g, None, level, at(110)),
            [Order::Stop {
                timeline_id: TimelineId::new("up")
            }]
        );
        value.set(1.0);
        assert!(seq.check(&g, None, level, at(120)).is_empty(), "held back");
        assert_eq!(
            seq.check(&g, None, level, at(150)),
            [play()],
            "deferred, not lost"
        );
        assert!(seq.check(&g, None, level, at(200)).is_empty(), "once");
    }

    #[test]
    fn a_scene_going_on_program_and_a_timeline_ending_play_it() {
        let g = graph(
            vec![
                TimelineTrigger::Scene {
                    scene_id: SceneId::new("s"),
                    edge: SceneEdge::Shown,
                    delay_ms: 500,
                },
                TimelineTrigger::Timeline {
                    timeline_id: TimelineId::new("intro"),
                    edge: TimelineEdge::Ends,
                    delay_ms: 0,
                },
            ],
            Vec::new(),
        );
        let mut seq = Sequences::default();
        let now = Instant::now();
        let s = SceneId::new("s");
        assert!(seq.check(&g, None, |_| None, now).is_empty());
        assert_eq!(
            seq.check(&g, Some(&s), |_| None, now),
            [Order::Play {
                timeline_id: TimelineId::new("up"),
                delay_ms: 500
            }]
        );
        seq.timeline_event(&TimelineId::new("intro"), TimelineEdge::Ends);
        assert_eq!(seq.check(&g, Some(&s), |_| None, now), [play()]);
        assert!(seq.check(&g, Some(&s), |_| None, now).is_empty(), "once");
    }

    #[test]
    fn a_wait_comes_due_and_reports_what_is_left() {
        let mut seq = Sequences::default();
        let now = Instant::now();
        let id = TimelineId::new("up");
        seq.wait(&id, SceneId::new("s"), 300, now);
        assert_eq!(seq.waiting(now)[0].due_in_ms, 300);
        assert!(seq.due(now + Duration::from_millis(100)).is_empty());
        assert_eq!(
            seq.due(now + Duration::from_millis(300)),
            std::slice::from_ref(&id)
        );
        assert!(!seq.is_waiting());
        seq.wait(&id, SceneId::new("s"), 300, now);
        seq.cancel(&id);
        assert!(!seq.is_waiting());
    }
}
