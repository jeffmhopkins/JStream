//! The timelines playing ([ADR-0115](../../../docs/adr/0115-timelines-on-the-wire.md),
//! ADR-0099 decision 4): studio state, like the program scene, which
//! `timeline.play`, `timeline.stop` and `timeline.seek` change and `studio.get` reports.
//!
//! A timeline's cues fire on the animation clock: the media clock the sources and the
//! render thread draw on (ADR-0099 decision 2), in nanoseconds here. Each playing timeline
//! keeps when it started, so a cue's time is fixed then, and how far its cues have been
//! fired, so each fires once a pass however the ticks fall. The schedule itself is
//! `jstream-anim`'s [`Timeline::due`]; the engine runs what it gives.

use std::collections::BTreeMap;

use jstream_anim::{Cue, CueAction, Fire, Looping, Timeline};
use jstream_media::{MediaTime, Rounding, Timebase};
use jstream_protocol::{
    Action, ClientId, PlayingTimeline, RequestId, SceneId, TimelineId, TimelineInfo, TimelineLoop,
};

/// The most rounds of cues one tick runs. A cue that plays a timeline starts it at the
/// cue's own time, so its first cues are due in the same tick and run in the next round;
/// timelines that start each other at 0 would never stop, so a tick stops after this
/// many rounds and the rest wait for the next tick, at least [`CAPPED_WAIT_NS`] later.
pub(crate) const MAX_ROUNDS: usize = 16;

/// How long the next tick waits, at least, after a tick ran all [`MAX_ROUNDS`] rounds
/// with cues still due: one frame at 60 fps. A timeline that plays itself at 0 restarts
/// at that cue's time, so its cue is due again at once; this bounds it to
/// `MAX_ROUNDS` cues a frame instead of a busy loop.
pub(crate) const CAPPED_WAIT_NS: i64 = 16_666_667;

const NS_PER_MS: i64 = 1_000_000;

/// A cue's action, for the schedule: what a seek keeps the last of
/// ([`Action::cue_state`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CueAct(pub(crate) Option<Action>);

impl CueAction for CueAct {
    type Key = String;
    fn state_key(&self) -> Option<String> {
        self.0.as_ref().and_then(Action::cue_state)
    }
}

/// A protocol timeline as `jstream-anim` schedules it.
pub(crate) fn schedule(info: &TimelineInfo) -> Timeline<CueAct> {
    Timeline {
        duration_ms: info.duration_ms,
        looping: match info.looping {
            TimelineLoop::Once => Looping::Once,
            TimelineLoop::Loop => Looping::Loop,
            TimelineLoop::Range { from_ms, to_ms } => Looping::Range { from_ms, to_ms },
        },
        cues: info
            .cues
            .iter()
            .map(|cue| Cue {
                at_ms: cue.at_ms,
                action: CueAct(cue.action()),
            })
            .collect(),
    }
}

/// A time on the media clock in nanoseconds.
pub(crate) fn ns(time: MediaTime) -> i64 {
    time.rescale(Timebase::NANOS, Rounding::Down)
        .map(MediaTime::ticks)
        .unwrap_or(i64::MAX)
}

/// A time in nanoseconds on the media clock.
pub(crate) fn media_time(ns: i64) -> MediaTime {
    MediaTime::new(ns, Timebase::NANOS)
}

/// One timeline playing.
#[derive(Clone, Debug)]
pub(crate) struct Playing {
    /// The scene that holds it.
    pub(crate) scene_id: SceneId,
    /// When its pass 0 started, on the animation clock, in nanoseconds. A seek to `t`
    /// starts it `t` earlier.
    pub(crate) started_ns: i64,
    /// Its cues before this time have fired.
    pub(crate) fired_to_ns: i64,
    /// The pass it was last seen on, so a loop reports each new pass once.
    pub(crate) pass: u64,
    /// The client whose call started it, which its cues' actions are credited to.
    pub(crate) client_id: ClientId,
    /// That call's request ID.
    pub(crate) request_id: RequestId,
}

/// What one tick found for a timeline besides its cues.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    /// It went on to pass `pass`, at `position_ms` into it.
    Looped { pass: u64, position_ms: u32 },
    /// It reached its end, after its last cue: a `once` timeline has only pass 0.
    Ended { position_ms: u32 },
}

/// The timelines playing, by ID.
#[derive(Debug, Default)]
pub(crate) struct Timelines {
    playing: BTreeMap<TimelineId, Playing>,
}

impl Timelines {
    /// Starts `id` from `from_ms` at `now_ns`, replacing any play of it: its cues from
    /// `from_ms` on fire from the next tick.
    pub(crate) fn play(
        &mut self,
        id: &TimelineId,
        scene_id: SceneId,
        from_ms: u32,
        now_ns: i64,
        client_id: ClientId,
        request_id: RequestId,
    ) {
        self.playing.insert(
            id.clone(),
            Playing {
                scene_id,
                started_ns: now_ns.saturating_sub(i64::from(from_ms) * NS_PER_MS),
                fired_to_ns: now_ns,
                pass: 0,
                client_id,
                request_id,
            },
        );
    }

    /// Stops `id`, returning how it was playing, or `None` if it wasn't.
    pub(crate) fn stop(&mut self, id: &TimelineId) -> Option<Playing> {
        self.playing.remove(id)
    }

    /// How `id` is playing, if it is.
    pub(crate) fn get(&self, id: &TimelineId) -> Option<&Playing> {
        self.playing.get(id)
    }

    /// The IDs playing.
    pub(crate) fn ids(&self) -> Vec<TimelineId> {
        self.playing.keys().cloned().collect()
    }

    /// Whether any timeline plays.
    pub(crate) fn is_empty(&self) -> bool {
        self.playing.is_empty()
    }

    /// The cues of `id` due up to `now_ns`, in the order they fire, and whether it
    /// looped or ended; marks them fired. Ending stops it. `None` if it isn't playing.
    pub(crate) fn advance(
        &mut self,
        id: &TimelineId,
        timeline: &Timeline<CueAct>,
        now_ns: i64,
    ) -> Option<(Vec<Fire>, Option<Step>)> {
        let playing = self.playing.get_mut(id)?;
        // `due` takes a half-open window, so a cue exactly at `now_ns` fires now.
        let to_ns = now_ns.saturating_add(1);
        let fires = if to_ns > playing.fired_to_ns {
            timeline.due(playing.started_ns, playing.fired_to_ns, to_ns)
        } else {
            Vec::new()
        };
        playing.fired_to_ns = playing.fired_to_ns.max(to_ns);
        let position = timeline.position(playing.started_ns, now_ns);
        let step = if position.ended {
            self.playing.remove(id);
            Some(Step::Ended {
                position_ms: timeline.duration_ms,
            })
        } else if position.pass > playing.pass {
            playing.pass = position.pass;
            Some(Step::Looped {
                pass: position.pass,
                position_ms: millis(position.ms),
            })
        } else {
            None
        };
        Some((fires, step))
    }

    /// When `id` next needs a tick, on the animation clock in nanoseconds: its next cue
    /// or the end of its pass, whichever is first. `None` if it isn't playing.
    pub(crate) fn next_due(&self, id: &TimelineId, timeline: &Timeline<CueAct>) -> Option<i64> {
        let playing = self.playing.get(id)?;
        let boundary = boundary(timeline, playing.started_ns, playing.pass);
        let cue = timeline
            .due(
                playing.started_ns,
                playing.fired_to_ns,
                boundary.saturating_add(1),
            )
            .first()
            .map(|fire| fire.at_ns);
        Some(cue.unwrap_or(boundary).min(boundary))
    }

    /// Each timeline playing and where it is at `now_ns`, ordered by ID. `timeline`
    /// finds each one's schedule; one it doesn't find is left out.
    pub(crate) fn report(
        &self,
        now_ns: i64,
        mut timeline: impl FnMut(&TimelineId) -> Option<Timeline<CueAct>>,
    ) -> Vec<PlayingTimeline> {
        self.playing
            .iter()
            .filter_map(|(id, playing)| {
                let position = timeline(id)?.position(playing.started_ns, now_ns);
                Some(PlayingTimeline {
                    timeline_id: id.clone(),
                    scene_id: playing.scene_id.clone(),
                    position_ms: millis(position.ms),
                    pass: position.pass,
                })
            })
            .collect()
    }
}

/// Milliseconds, as the wire carries a position.
pub(crate) fn millis(ms: f64) -> u32 {
    ms.clamp(0.0, f64::from(u32::MAX)) as u32
}

/// When pass `pass` of a timeline that started at `started_ns` ends: where a `Once`
/// timeline ends, or where a loop jumps back.
pub(crate) fn boundary(timeline: &Timeline<CueAct>, started_ns: i64, pass: u64) -> i64 {
    let ms = |ms: u32| i64::from(ms) * NS_PER_MS;
    let pass = i64::try_from(pass).unwrap_or(i64::MAX);
    let at = match timeline.looping {
        Looping::Once => ms(timeline.duration_ms),
        Looping::Loop => ms(timeline.duration_ms).saturating_mul(pass.saturating_add(1)),
        Looping::Range { from_ms, to_ms } => {
            ms(to_ms).saturating_add(ms(to_ms - from_ms).saturating_mul(pass))
        }
    };
    started_ns.saturating_add(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_protocol::{ItemId, TimelineCue};

    fn info(duration_ms: u32, looping: TimelineLoop, at: &[u32]) -> TimelineInfo {
        TimelineInfo {
            timeline_id: TimelineId::new("t"),
            name: String::new(),
            duration_ms,
            looping,
            cues: at
                .iter()
                .map(|&at_ms| {
                    TimelineCue::new(
                        at_ms,
                        &Action::ItemToggle {
                            item_id: ItemId::new("i"),
                            scene_id: None,
                        },
                    )
                })
                .collect(),
        }
    }

    fn timelines(from_ms: u32, now_ns: i64) -> Timelines {
        let mut timelines = Timelines::default();
        timelines.play(
            &TimelineId::new("t"),
            SceneId::new("s"),
            from_ms,
            now_ns,
            ClientId::new("c"),
            RequestId::new("r"),
        );
        timelines
    }

    const MS: i64 = NS_PER_MS;

    /// A protocol timeline `problem` passes is one `jstream-anim` accepts, so the two
    /// checks agree.
    #[test]
    fn a_timeline_the_protocol_accepts_schedules() {
        let range = TimelineLoop::Range {
            from_ms: 200,
            to_ms: 800,
        };
        for t in [
            info(4000, TimelineLoop::Once, &[0, 1500, 4000]),
            info(1000, TimelineLoop::Loop, &[0, 999]),
            info(1000, range, &[0, 200, 799]),
        ] {
            assert_eq!(t.problem(), None);
            assert_eq!(schedule(&t).check(), Ok(()));
        }
    }

    #[test]
    fn each_cue_fires_once_and_a_once_timeline_ends() {
        let t = schedule(&info(1000, TimelineLoop::Once, &[0, 500, 1000]));
        let id = TimelineId::new("t");
        let mut playing = timelines(0, 10 * MS);
        let (fires, step) = playing.advance(&id, &t, 10 * MS).unwrap_or_default();
        assert_eq!(fires.iter().map(|f| f.cue).collect::<Vec<_>>(), [0]);
        assert_eq!(step, None);
        assert_eq!(playing.next_due(&id, &t), Some(510 * MS));
        let (fires, _) = playing.advance(&id, &t, 400 * MS).unwrap_or_default();
        assert!(fires.is_empty());
        let (fires, step) = playing.advance(&id, &t, 2000 * MS).unwrap_or_default();
        assert_eq!(fires.iter().map(|f| f.cue).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(step, Some(Step::Ended { position_ms: 1000 }));
        assert!(playing.is_empty());
    }

    #[test]
    fn a_loop_reports_each_pass_and_its_next_boundary() {
        let t = schedule(&info(1000, TimelineLoop::Loop, &[250]));
        let id = TimelineId::new("t");
        let mut playing = timelines(0, 0);
        assert_eq!(playing.next_due(&id, &t), Some(250 * MS));
        let _ = playing.advance(&id, &t, 300 * MS);
        assert_eq!(playing.next_due(&id, &t), Some(1000 * MS));
        let (fires, step) = playing.advance(&id, &t, 1300 * MS).unwrap_or_default();
        assert_eq!(
            fires.iter().map(|f| (f.cue, f.pass)).collect::<Vec<_>>(),
            [(0, 1)]
        );
        assert_eq!(
            step,
            Some(Step::Looped {
                pass: 1,
                position_ms: 300
            })
        );
    }

    #[test]
    fn a_seek_starts_the_clock_earlier_and_fires_from_there() {
        let t = schedule(&info(1000, TimelineLoop::Once, &[100, 600]));
        let id = TimelineId::new("t");
        let mut playing = timelines(500, 2000 * MS);
        let report = playing.report(2000 * MS, |_| Some(t.clone()));
        assert_eq!(report[0].position_ms, 500);
        let (fires, _) = playing.advance(&id, &t, 2100 * MS).unwrap_or_default();
        assert_eq!(fires.iter().map(|f| f.cue).collect::<Vec<_>>(), [1]);
    }
}
