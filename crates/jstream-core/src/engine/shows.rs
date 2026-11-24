//! The show running (ADR-0210): studio state, like the program scene, which the
//! `show.*` actions change, the live ticker advances and `studio.get` reports.
//!
//! The schedule is `jstream-anim`'s [`Runner`]: a pure state machine over a [`Plan`]
//! built from the collection's [`ShowInfo`] on every call, so an edit to a running show
//! takes effect at once. This file turns its [`Effect`]s into the calls a key, a pad or
//! MCP would make (`scene.switch`, `scene.set_keyframe`, `timeline.play`, a cue's
//! action), each at the exact time it fell due, and into `show.changed` events.

use std::task::Waker;

use jstream_anim::show::{
    Bookmark, Effect, EndOfShow, KeyframeMark, Landing, Leave, LoopRegion, OverlayClip, Plan,
    ProgramClip, Runner, SequenceClip, ShowEvent, ShowKeys, Status, Step,
};
use jstream_graph::Graph;
use jstream_protocol::{
    Action, ClientId, EntityKind, Event, LoopLeave, ProtocolError, RequestId, RunningShow, SceneId,
    ShowChange, ShowEnd, ShowId, ShowInfo, ShowState, TimelineId, TransitionPresetId,
};

use super::State;
use crate::timelines::CueAct;

/// The engine's types, for the runner.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Keys;

impl ShowKeys for Keys {
    type Scene = SceneId;
    type Timeline = TimelineId;
    type Bookmark = String;
    type Loop = String;
    type Track = String;
    type Preset = TransitionPresetId;
    type Action = CueAct;
}

/// The show running.
#[derive(Debug)]
pub(super) struct Run {
    /// Which show.
    pub(super) show_id: ShowId,
    /// Its play head.
    pub(super) runner: Runner<TimelineId>,
    /// The client and request that started it, which its effects are credited to.
    pub(super) by: (ClientId, RequestId),
}

/// The plan of `info`, as the runner reads it: the overlay tracks' clips flattened by
/// start, a `timeline.play` cue as a sequence clip as long as its sequence, and every
/// other cue as an event.
pub(super) fn plan(info: &ShowInfo, graph: &Graph) -> Plan<Keys> {
    let program = info
        .program
        .iter()
        .map(|clip| ProgramClip {
            scene: clip.scene_id.clone(),
            length_ms: clip.length_ms,
            transition: clip.preset_id.clone(),
            // Media seeking on a jump comes with `source.seek`.
            timed: false,
        })
        .collect();
    let mut overlays: Vec<OverlayClip<Keys>> = info
        .overlays
        .iter()
        .flat_map(|track| {
            track.clips.iter().map(|clip| OverlayClip {
                track: track.track_id.clone(),
                scene: clip.scene_id.clone(),
                start_ms: clip.start_ms,
                length_ms: clip.length_ms,
                fade_in_ms: clip.fade_in_ms,
                fade_out_ms: clip.fade_out_ms,
                timed: false,
            })
        })
        .collect();
    overlays.sort_by_key(|clip| clip.start_ms);
    let keyframes = info
        .keyframes
        .iter()
        .map(|marker| KeyframeMark {
            at_ms: marker.at_ms,
            keyframe: marker.keyframe_id.clone(),
            preset: marker.preset_id.clone(),
        })
        .collect();
    let mut sequences = Vec::new();
    let mut events = Vec::new();
    for cue in info.sequences.iter().flat_map(|track| track.cues.iter()) {
        let action = cue.action();
        if let Some(Action::TimelinePlay { timeline_id, .. }) = &action {
            let length_ms = graph
                .state()
                .timeline(timeline_id)
                .map_or(1, |(_, timeline)| timeline.duration_ms.max(1));
            sequences.push(SequenceClip {
                timeline: timeline_id.clone(),
                start_ms: cue.at_ms,
                length_ms,
            });
        } else {
            events.push(ShowEvent {
                at_ms: cue.at_ms,
                action: CueAct(action),
            });
        }
    }
    sequences.sort_by_key(|clip| clip.start_ms);
    events.sort_by_key(|event| event.at_ms);
    let bookmarks = info
        .bookmarks
        .iter()
        .map(|b| Bookmark {
            id: b.bookmark_id.clone(),
            at_ms: b.at_ms,
            pausing: b.pausing,
            arrive: b.preset_id.clone(),
            limit_ms: b.continue_after_ms,
        })
        .collect();
    let loops = info
        .loops
        .iter()
        .map(|l| LoopRegion {
            id: l.loop_id.clone(),
            from_ms: l.start_ms,
            to_ms: l.end_ms,
            leave: match l.leave {
                LoopLeave::FinishPass => Leave::FinishPass,
                _ => Leave::AtOnce,
            },
            wrap: l.preset_id.clone(),
        })
        .collect();
    let end = match &info.end {
        ShowEnd::Loop => EndOfShow::Loop,
        ShowEnd::GoTo { bookmark_id } => EndOfShow::GoTo(bookmark_id.clone()),
        _ => EndOfShow::Stop,
    };
    Plan {
        program,
        overlays,
        keyframes,
        sequences,
        events,
        bookmarks,
        loops,
        end,
        default_transition: info.preset_id.clone(),
    }
}

impl State {
    /// The running show's plan, or `None` with no show running or its show gone.
    fn show_plan(&self) -> Option<Plan<Keys>> {
        let run = self.show.as_ref()?;
        let info = self.graph.show(&run.show_id)?;
        Some(plan(info, &self.graph))
    }

    /// The animation clock now, in nanoseconds.
    fn show_now(&self) -> i64 {
        crate::timelines::ns(self.action_now())
    }

    /// Runs a `show.*` action. `None` for any other action.
    pub(super) fn show_action(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        action: &Action,
        wake: &mut Vec<Waker>,
    ) -> Option<Result<(), ProtocolError>> {
        if let Some((field, reason)) = action.show_problem() {
            return Some(Err(ProtocolError::invalid_argument(field, reason)));
        }
        let done = match action {
            Action::ShowPlay {
                show_id,
                bookmark_id,
                ..
            } => self.show_play(client_id, request_id, show_id, bookmark_id.as_deref(), wake),
            Action::ShowStop => {
                self.show_stop(ShowChange::Stopped, wake);
                Ok(())
            }
            Action::ShowPause => {
                self.show_step(wake, |runner, plan, now| runner.pause(plan, now));
                Ok(())
            }
            Action::ShowContinue => {
                if !self.in_transition() {
                    self.show_step(wake, |runner, plan, now| runner.continue_(plan, now));
                }
                Ok(())
            }
            Action::ShowBack => {
                self.show_step(wake, |runner, plan, now| runner.back(plan, now));
                Ok(())
            }
            Action::ShowSkip => {
                self.show_step(wake, |runner, plan, now| runner.skip(plan, now));
                Ok(())
            }
            Action::ShowGoTo {
                show_id,
                bookmark_id,
            } => {
                if self.graph.show(show_id).is_none() {
                    return Some(Err(ProtocolError::NotFound {
                        entity: EntityKind::Show,
                        id: show_id.to_string(),
                    }));
                }
                if self
                    .show
                    .as_ref()
                    .is_some_and(|run| run.show_id == *show_id)
                {
                    let id = bookmark_id.clone();
                    self.show_step(wake, move |runner, plan, now| match plan.bookmark(&id) {
                        Some(b) => runner.go_to(plan, b, now),
                        None => Vec::new(),
                    });
                }
                Ok(())
            }
            Action::ShowGoToNumber { number } => {
                let n = usize::from(*number).saturating_sub(1);
                self.show_step(wake, move |runner, plan, now| {
                    if n < plan.bookmarks.len() {
                        runner.go_to(plan, n, now)
                    } else {
                        Vec::new()
                    }
                });
                Ok(())
            }
            Action::ShowSeek { at_ms } => {
                let at = *at_ms;
                self.show_step(wake, move |runner, plan, now| {
                    runner.jump(plan, at, Landing::PlayOn, now)
                });
                Ok(())
            }
            Action::ShowArm { at_ms } => {
                if let Some(run) = self.show.as_mut() {
                    run.runner.arm(*at_ms);
                }
                Ok(())
            }
            _ => return None,
        };
        Some(done)
    }

    /// Whether a scene switch's transition is still playing, so Continue is ignored
    /// (ADR-0210 decision 5).
    fn in_transition(&self) -> bool {
        let now = self.live.now();
        self.studio
            .running()
            .is_some_and(|running| running.elapsed_ms(now) < running.run.duration_ms as f32)
    }

    /// `show.play`: stops the show running, if another, and starts `show_id` at its
    /// start or at `bookmark_id`.
    fn show_play(
        &mut self,
        client_id: &ClientId,
        request_id: &RequestId,
        show_id: &ShowId,
        bookmark_id: Option<&str>,
        wake: &mut Vec<Waker>,
    ) -> Result<(), ProtocolError> {
        // Play on the show paused by hand plays it on from where it holds.
        if bookmark_id.is_none()
            && self
                .show
                .as_ref()
                .is_some_and(|run| &run.show_id == show_id && run.runner.held())
        {
            self.show_step(wake, |runner, plan, now| runner.unpause(plan, now));
            return Ok(());
        }
        let info = self
            .graph
            .show(show_id)
            .ok_or_else(|| ProtocolError::NotFound {
                entity: EntityKind::Show,
                id: show_id.to_string(),
            })?;
        let plan = plan(info, &self.graph);
        let bookmark =
            match bookmark_id {
                None => None,
                Some(id) => Some(plan.bookmarks.iter().position(|b| b.id == id).ok_or_else(
                    || {
                        ProtocolError::invalid_argument(
                            "bookmark_id",
                            "must name one of the show's bookmarks",
                        )
                    },
                )?),
            };
        if self.show.is_some() {
            self.show_stop(ShowChange::Stopped, wake);
        }
        let before = self.studio.program().cloned();
        let now = self.show_now();
        let mut runner = Runner::new();
        let steps = match bookmark {
            Some(i) => runner.start_at_bookmark(&plan, i, now),
            None => runner.start(&plan, 0, Landing::PlayOn, now),
        };
        self.show = Some(Run {
            show_id: show_id.clone(),
            runner,
            by: (client_id.clone(), request_id.clone()),
        });
        self.emit_show(ShowChange::Started, None, None, wake);
        self.run_show_steps(&plan, steps, wake);
        // A show that starts on the scene already on program starts its timers again, as
        // the cut to it would have.
        if let Some(program) = before.filter(|p| self.studio.program() == Some(p)) {
            self.restart_timers(&program);
        }
        self.show_settle(wake);
        Ok(())
    }

    /// Stops the running show, if any: the sequences it started stop and its overlays
    /// go, program keeps its scene. Emits `show.changed` with `change`.
    pub(super) fn show_stop(&mut self, change: ShowChange, wake: &mut Vec<Waker>) {
        let Some(plan) = self.show_plan() else {
            if let Some(run) = self.show.take() {
                self.emit_stopped(&run, change, wake);
            }
            return;
        };
        let now = self.show_now();
        let steps = match self.show.as_mut() {
            Some(run) => run.runner.stop(&plan, now),
            None => return,
        };
        self.run_show_steps(&plan, steps, wake);
        if let Some(run) = self.show.take() {
            self.emit_stopped(&run, change, wake);
        }
    }

    fn emit_stopped(&mut self, run: &Run, change: ShowChange, wake: &mut Vec<Waker>) {
        let at_ms = run.runner.position_ms(self.show_now());
        self.subscribers.emit(
            &Event::ShowChanged {
                show_id: run.show_id.clone(),
                change,
                running: false,
                at_ms,
                bookmark_id: None,
                loop_id: None,
            },
            wake,
        );
    }

    /// Runs `call` on the running show's runner, then its effects, and sets the ticker.
    /// Does nothing with no show running.
    fn show_step(
        &mut self,
        wake: &mut Vec<Waker>,
        call: impl FnOnce(&mut Runner<TimelineId>, &Plan<Keys>, i64) -> Vec<Step<Keys>>,
    ) {
        let Some(plan) = self.show_plan() else {
            return;
        };
        let now = self.show_now();
        let steps = match self.show.as_mut() {
            Some(run) => call(&mut run.runner, &plan, now),
            None => return,
        };
        self.run_show_steps(&plan, steps, wake);
        self.show_settle(wake);
    }

    /// Restarts every `timer` source `scene` draws (its countdowns among them), at the
    /// action's time (`LiveSources::restart`).
    fn restart_timers(&mut self, scene: &SceneId) {
        let state = self.graph.state();
        let timers: std::collections::BTreeSet<_> = state
            .tree_items(state.family_of(scene))
            .into_iter()
            .filter_map(|item| state.item(item).map(|(_, item)| item.source_id().clone()))
            .filter(|source| {
                state.source(source).is_some_and(|s| {
                    matches!(
                        s.kind(),
                        jstream_protocol::SourceKind::Builtin { kind } if kind == "timer"
                    )
                })
            })
            .collect();
        let at = self.action_now();
        for source in timers {
            self.live.restart(&source, at);
        }
    }

    /// After a show call: runs what fell due, ticks the timelines (a sequence the show
    /// started ran as of its step's time, which skips its own tick), and wakes the live
    /// ticker so it plans from the show's new next point.
    fn show_settle(&mut self, wake: &mut Vec<Waker>) {
        self.show_tick(wake);
        if self.cue_at.is_none() {
            self.timeline_tick(wake);
        }
        self.start_ticker();
    }

    /// Advances the running show to now and runs what fell due. Returns when it next
    /// needs a tick, on the animation clock.
    pub(super) fn show_tick(&mut self, wake: &mut Vec<Waker>) -> Option<jstream_media::MediaTime> {
        let plan = self.show_plan()?;
        let now = crate::timelines::ns(self.live.now());
        let steps = self.show.as_mut()?.runner.tick(&plan, now);
        self.run_show_steps(&plan, steps, wake);
        let run = self.show.as_ref()?;
        if matches!(run.runner.status(), Status::Ended | Status::Stopped) {
            return None;
        }
        run.runner.next_due(&plan).map(crate::timelines::media_time)
    }

    /// Whether the running show needs the live ticker.
    pub(super) fn show_ticking(&self) -> bool {
        self.show.as_ref().is_some_and(|run| {
            matches!(run.runner.status(), Status::Playing | Status::Paused { .. })
        })
    }

    /// A scene went on program by hand while a show runs: the show goes off script.
    pub(super) fn show_hand_switch(&mut self, wake: &mut Vec<Waker>) {
        if self.show_switching {
            return;
        }
        let now = self.show_now();
        let Some(run) = self.show.as_mut() else {
            return;
        };
        if !matches!(run.runner.status(), Status::Playing | Status::Paused { .. }) {
            return;
        }
        run.runner.off_script(now);
        self.emit_show(ShowChange::OffScript, None, None, wake);
    }

    /// Runs the runner's steps, each as of its own time on the animation clock.
    fn run_show_steps(&mut self, plan: &Plan<Keys>, steps: Vec<Step<Keys>>, wake: &mut Vec<Waker>) {
        let Some(by) = self.show.as_ref().map(|run| run.by.clone()) else {
            return;
        };
        for step in steps {
            let outer = self
                .cue_at
                .replace(crate::timelines::media_time(step.at_ns));
            self.run_show_effect(plan, step.effect, &by, wake);
            self.cue_at = outer;
        }
    }

    fn run_show_effect(
        &mut self,
        plan: &Plan<Keys>,
        effect: Effect<Keys>,
        by: &(ClientId, RequestId),
        wake: &mut Vec<Waker>,
    ) {
        let (client_id, request_id) = (&by.0, &by.1);
        // A clip naming something the collection no longer holds does nothing.
        match effect {
            Effect::Program {
                scene, transition, ..
            } => {
                self.show_switching = true;
                let _ = self.switch(
                    client_id,
                    request_id,
                    scene,
                    transition.as_ref(),
                    Vec::new(),
                    wake,
                );
                self.show_switching = false;
            }
            Effect::Seek { .. } => {}
            Effect::Keyframe {
                keyframe, preset, ..
            } => {
                let Some(program) = self.studio.program().cloned() else {
                    return;
                };
                let state = self.graph.state();
                if state.scene(&program).is_none() {
                    return;
                }
                let family = state.family_of(&program).clone();
                let target = keyframe.clone().unwrap_or_else(|| family.clone());
                let ours = state
                    .scene(&target)
                    .is_some_and(|_| *state.family_of(&target) == family);
                if ours {
                    // A keyframe of the scene on program: program itself moves to it, as a
                    // switch within the family (a placed keyframe only changes how other
                    // scenes draw it).
                    if target != program {
                        self.show_switching = true;
                        let _ = self.switch(
                            client_id,
                            request_id,
                            target,
                            preset.as_ref(),
                            Vec::new(),
                            wake,
                        );
                        self.show_switching = false;
                    }
                } else {
                    let _ =
                        self.set_keyframe(client_id, request_id, program, keyframe, preset, wake);
                }
            }
            Effect::OverlayOn { .. } | Effect::OverlayOff { .. } => {}
            Effect::SequencePlay {
                timeline,
                offset_ms,
                ..
            } => {
                let from = (offset_ms > 0).then_some(offset_ms);
                let _ = self.timeline_play(client_id, request_id, &timeline, from, wake);
            }
            Effect::SequenceStop { timeline, .. } => {
                let _ = self.timeline_stop(&timeline, wake);
            }
            Effect::Action { action, .. } => {
                // A cue that drives a show would re-enter the runner; a show's own cues
                // never do.
                if let Some(action) = action.0
                    && !action.name().starts_with("show.")
                {
                    let _ = self.action(client_id, request_id, action, wake);
                }
            }
            Effect::Paused { bookmark, .. } => {
                let id = bookmark
                    .and_then(|b| plan.bookmarks.get(b))
                    .map(|b| b.id.clone());
                self.emit_show(ShowChange::Paused, id, None, wake);
            }
            Effect::Resumed { .. } => self.emit_show(ShowChange::Continued, None, None, wake),
            Effect::Jumped { to_ms, .. } => {
                let id = plan
                    .bookmarks
                    .iter()
                    .find(|b| b.at_ms == to_ms)
                    .map(|b| b.id.clone());
                self.emit_show(ShowChange::Jumped, id, None, wake);
            }
            Effect::Looped { region, .. } => {
                let id = plan.loops.get(region).map(|l| l.id.clone());
                self.emit_show(ShowChange::Looped, None, id, wake);
            }
            Effect::Ended => self.emit_show(ShowChange::Ended, None, None, wake),
        }
    }

    /// Emits `show.changed` for the running show.
    fn emit_show(
        &mut self,
        change: ShowChange,
        bookmark_id: Option<String>,
        loop_id: Option<String>,
        wake: &mut Vec<Waker>,
    ) {
        let now = self.show_now();
        let Some(run) = self.show.as_ref() else {
            return;
        };
        let event = Event::ShowChanged {
            show_id: run.show_id.clone(),
            change,
            running: !matches!(run.runner.status(), Status::Ended | Status::Stopped),
            at_ms: run.runner.position_ms(now),
            bookmark_id,
            loop_id,
        };
        self.subscribers.emit(&event, wake);
    }

    /// The running show, as `studio.get` reports it.
    pub(super) fn show_report(&self) -> Option<Box<RunningShow>> {
        let run = self.show.as_ref()?;
        let plan = self.show_plan()?;
        let now = crate::timelines::ns(self.live.now());
        let at_ms = run.runner.position_ms(now);
        let waited = |since_ns: i64| {
            u32::try_from(now.saturating_sub(since_ns).max(0) / 1_000_000).unwrap_or(u32::MAX)
        };
        let (state, bookmark_id, waited_ms) = match run.runner.status() {
            Status::Playing => (ShowState::Playing, None, None),
            Status::Paused { bookmark, since_ns } => (
                ShowState::Paused,
                bookmark
                    .and_then(|b| plan.bookmarks.get(b))
                    .map(|b| b.id.clone()),
                Some(waited(since_ns)),
            ),
            Status::OffScript { since_ns } => (ShowState::OffScript, None, Some(waited(since_ns))),
            Status::Ended | Status::Stopped => return None,
        };
        let off_script = (state == ShowState::OffScript)
            .then(|| self.studio.program().cloned())
            .flatten();
        let (loop_id, pass) = run
            .runner
            .looping()
            .and_then(|(region, pass)| {
                let l = plan.loops.get(region)?;
                Some((
                    Some(l.id.clone()),
                    Some(u32::try_from(pass).unwrap_or(u32::MAX)),
                ))
            })
            .unwrap_or((None, None));
        Some(Box::new(RunningShow {
            show_id: run.show_id.clone(),
            at_ms,
            state,
            bookmark_id,
            waited_ms,
            off_script,
            loop_id,
            pass,
            armed_ms: run.runner.armed_ms(),
            rehearse: false,
        }))
    }

    /// After a batch, undo or redo: a running show whose show was deleted stops.
    pub(super) fn reconcile_show(&mut self, wake: &mut Vec<Waker>) {
        let gone = self
            .show
            .as_ref()
            .is_some_and(|run| self.graph.show(&run.show_id).is_none());
        if gone {
            self.show_stop(ShowChange::Stopped, wake);
        }
    }
}
