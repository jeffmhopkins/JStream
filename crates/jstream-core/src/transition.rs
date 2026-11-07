//! Transitions on the control thread (ADR-0029): which preset a switch uses, and the
//! plan the render thread samples.
//!
//! [`resolve`] picks a switch's preset: the one the switch names, else the new scene's
//! `transition_in`, else the old scene's `transition_out`, else the default. A `cut`, a
//! zero duration, or a preset whose kind this engine lacks becomes no [`Run`]: the switch
//! shows the new scene on the next frame.
//!
//! [`plan`] pairs what is on screen with the new scene's items and returns one
//! [`Track`] per item in drawing order: everything leaving first, then the new scene
//! bottom to top. `move` pairs items placing the same source, the k-th placement with the
//! k-th in stacking order, unless either one's glide is `never`; `fade` pairs nothing.
//! Between two members of one family (a scene and its keyframe scenes, ADR-0049 §3),
//! `move` pairs by item instead: each item with itself on the other side.
//! Everything else appears or disappears with its effect. The render thread only samples
//! the tracks (R-RT-002).
//!
//! A group that moves as one item between two members of a family carries its
//! children's own tracks ([`Inside`]), planned the same way inside the group's layer
//! with the group item's progress (ADR-0049 §3).

use std::sync::Arc;

use jstream_graph::Graph;
use jstream_media::MediaTime;
use jstream_protocol::{
    Easing, EntityKind, FADE, Glide, ItemId, ItemTransition, MOVE, ProtocolError, Reference,
    STINGER, SceneCamera, SceneId, Settings, SourceId, TransitionEffect, TransitionPresetId,
    TransitionStarted,
};
use jstream_render::transition::{Effect, Endpoint, Timing, Track};

use crate::motion::TrackMotion;
use crate::snapshot::SourceRef;

/// What a running transition does: its preset, resolved.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Run {
    pub(crate) preset_id: TransitionPresetId,
    pub(crate) pairs: bool,
    pub(crate) duration_ms: u32,
    pub(crate) easing: Easing,
    pub(crate) unmatched: TransitionEffect,
    pub(crate) stagger_ms: u32,
    /// For a `stinger`, what it plays over the switch; `None` for every other kind.
    pub(crate) stinger: Option<Stinger>,
}

/// A `stinger` transition's settings, resolved.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stinger {
    /// The scene drawn over the switch, if the preset names one.
    pub(crate) scene: Option<SceneId>,
    /// When the picture under it cuts to the new scene, in milliseconds from the start:
    /// at most the duration.
    pub(crate) cut_ms: u32,
}

/// The preset a switch from `from` to `to` uses (ADR-0029 §2), and what it runs: `None`
/// for a cut. Fails with `not_found` when `named` is not a preset.
pub(crate) fn resolve(
    graph: &Graph,
    from: Option<&SceneId>,
    to: &SceneId,
    named: Option<&TransitionPresetId>,
) -> Result<(TransitionPresetId, Option<Run>), ProtocolError> {
    let state = graph.state();
    if let Some(id) = named
        && state.preset(id).is_none()
    {
        return Err(ProtocolError::NotFound {
            entity: EntityKind::Transition,
            id: id.as_str().to_owned(),
        });
    }
    let id = named
        .or_else(|| state.scene(to).and_then(|scene| scene.transition_in()))
        .or_else(|| {
            from.and_then(|from| state.scene(from))
                .and_then(|scene| scene.transition_out())
        })
        .unwrap_or_else(|| state.default_preset())
        .clone();
    let Some(preset) = state.preset(&id) else {
        return Ok((id, None));
    };
    // `cut`, and any kind this engine can't run (a pack that isn't installed), cut.
    let pairs = match preset.kind() {
        // A Move between a 3D scene and a 2D one has no one space to move in, so it
        // fades (ADR-0094 decision 7).
        MOVE => from
            .is_none_or(|from| camera_of(graph, from).is_some() == camera_of(graph, to).is_some()),
        FADE | STINGER => false,
        _ => return Ok((id, None)),
    };
    if preset.duration_ms() == 0 {
        return Ok((id, None));
    }
    let settings = preset.settings();
    let stagger_ms = settings
        .get("stagger_ms")
        .and_then(|value| value.as_u64())
        .and_then(|ms| u32::try_from(ms).ok())
        .unwrap_or(0);
    let stinger = (preset.kind() == STINGER).then(|| Stinger {
        scene: settings
            .get("scene")
            .and_then(Reference::from_value)
            .and_then(|reference| match reference {
                Reference::Scene { scene_id } => Some(scene_id),
                _ => None,
            }),
        cut_ms: settings
            .get("cut_ms")
            .and_then(|value| value.as_u64())
            .and_then(|ms| u32::try_from(ms).ok())
            .unwrap_or(0)
            .min(preset.duration_ms()),
    });
    let run = Run {
        preset_id: id.clone(),
        pairs,
        duration_ms: preset.duration_ms(),
        easing: preset.easing(),
        unmatched: TransitionEffect::from_setting(settings.get("unmatched")),
        stagger_ms,
        stinger,
    };
    Ok((id, Some(run)))
}

/// One item as it is on screen, or as it will be: what it shows, the source it pairs by,
/// where it is, and its transition settings.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Visual {
    pub(crate) image: SourceRef,
    /// The item it draws, which pairing within a family matches.
    pub(crate) item_id: ItemId,
    /// The source the item places, which pairing matches.
    pub(crate) source_id: SourceId,
    pub(crate) at: Endpoint,
    /// The size of the image in pixels, which bounds its crop, when known.
    pub(crate) size: Option<[u32; 2]>,
    pub(crate) settings: ItemTransition,
    /// Its effects' kinds and settings in order (ADR-0065), empty without: two items
    /// pair only when theirs are the same kinds in the same order, with settings a Move
    /// can blend ([`effects_pair`]).
    pub(crate) effects: Vec<(String, Settings)>,
    /// Once a Move pairs it with an item whose effects' settings differ, the settings it
    /// blends them from, one per effect in order (ADR-0127); `None` for everything else.
    pub(crate) blend_from: Option<Arc<[Settings]>>,
    /// For a group that moves as one item within a family, its children's tracks
    /// inside its layer (ADR-0049 §3); `None` for everything else.
    pub(crate) inside: Option<Arc<Inside>>,
    /// The camera it is seen through: a planned scene's item takes its transition's,
    /// and what a switch takes from the screen keeps the one it was seen through.
    pub(crate) seen: Seen,
    /// Its motion modifiers (ADR-0099): its own on both sides, and once a Move pairs it
    /// with an item it leaves, that item's on the leaving side.
    pub(crate) motion: TrackMotion,
}

/// The camera a [`Visual`] is seen through (ADR-0094 decision 7).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum Seen {
    /// The camera its transition gives its track.
    #[default]
    AsPlanned,
    /// Taken from the screen mid-transition: the camera it was seen through then
    /// (`None` for 2D) and the depth run it was sorted in, both of which it keeps as it
    /// leaves, so a stinger's items stay over the scenes they covered.
    Held {
        camera: Option<SceneCamera>,
        depth_run: u32,
    },
}

/// The one camera every visual taken from the screen was seen through, or `Err` when
/// they differ (a Fade or a stinger between scenes with different cameras), so the
/// switch that takes them can't move one camera. `Ok(None)` for 2D, or nothing shown.
pub(crate) fn shared_camera(visuals: &[Visual]) -> Result<Option<SceneCamera>, ()> {
    let mut held = visuals.iter().map(|visual| match visual.seen {
        Seen::Held { camera, .. } => camera,
        Seen::AsPlanned => None,
    });
    let Some(first) = held.next() else {
        return Ok(None);
    };
    if held.all(|camera| camera == first) {
        Ok(first)
    } else {
        Err(())
    }
}

/// A group's children as a Move within a family draws them inside the group's layer
/// (ADR-0049 §3): their tracks, which follow the group item's progress, or, once a
/// switch retargets the Move, what of them was on screen then, held still.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Inside {
    /// The group scene, whose video chain runs over its layer.
    pub(crate) group: SceneId,
    /// The scene the group item is planned in, when the item's effects draw it: its box
    /// is planned again over the children's layer.
    pub(crate) boxed_in: Option<SceneId>,
    /// The children's tracks, in drawing order.
    pub(crate) planned: Planned,
}

/// What a transition starts from: a scene, planned from the collection as it is now, or
/// the items that were on screen when it retargeted a running one.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum From {
    Scene(SceneId),
    OnScreen(Arc<[Visual]>),
}

/// A transition in progress, kept in the studio state.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Running {
    pub(crate) from: From,
    pub(crate) to: SceneId,
    /// When it started, on the media clock.
    pub(crate) start: MediaTime,
    pub(crate) run: Run,
    /// Whether it switches between members of one family, so `move` pairs by item
    /// (ADR-0049 §3).
    pub(crate) by_item: bool,
    /// The camera it moves from (ADR-0094 decision 7): the scene it leaves's, or the one
    /// on screen when it cut a transition short. `None` from a 2D scene.
    pub(crate) from_camera: Option<SceneCamera>,
}

impl Running {
    /// Milliseconds from its start to `at`.
    pub(crate) fn elapsed_ms(&self, at: MediaTime) -> f32 {
        elapsed_ms(self.start, at)
    }
}

/// The camera scene `id` draws through (ADR-0094): its own, or a keyframe scene's
/// parent's. `None` for a 2D scene or a scene that doesn't exist.
pub(crate) fn camera_of(graph: &Graph, id: &SceneId) -> Option<SceneCamera> {
    let state = graph.state();
    state
        .scene(id)
        .and_then(|scene| state.effective_camera(scene))
}

/// Milliseconds from `start` to `at`, on the same clock.
pub(crate) fn elapsed_ms(start: MediaTime, at: MediaTime) -> f32 {
    ((at.as_secs_f64() - start.as_secs_f64()) * 1000.0) as f32
}

/// The planned transition: each item's track in drawing order, with what it shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Planned {
    pub(crate) tracks: Vec<(Visual, Track)>,
    /// When the last item is done, in milliseconds from the start.
    pub(crate) end_ms: f32,
}

impl Planned {
    /// What is on screen `elapsed_ms` after the start, as the starting point of a
    /// transition that retargets this one: every item still drawn, with the settings of
    /// the placement it came from. A group's children inside its layer are held where
    /// they are then.
    pub(crate) fn on_screen(&self, elapsed_ms: f32, canvas: [f32; 2]) -> Vec<Visual> {
        self.on_screen_seen(elapsed_ms, canvas, |visual, _| visual.seen)
    }

    /// [`Self::on_screen`], each visual seen through the camera `seen` gives its track.
    pub(crate) fn on_screen_seen(
        &self,
        elapsed_ms: f32,
        canvas: [f32; 2],
        seen: impl Fn(&Visual, &Track) -> Seen,
    ) -> Vec<Visual> {
        self.tracks
            .iter()
            .filter_map(|(visual, track)| {
                track.sample(elapsed_ms, canvas).map(|at| Visual {
                    at,
                    seen: seen(visual, track),
                    // Taken from the screen mid-blend, it looks as it does then, which
                    // a Move from here blends on from (ADR-0127).
                    effects: blended_look(visual, track, elapsed_ms)
                        .unwrap_or_else(|| visual.effects.clone()),
                    blend_from: None,
                    inside: visual.inside.as_ref().map(|inside| {
                        Arc::new(Inside {
                            planned: inside.planned.held(elapsed_ms, canvas),
                            ..Inside::clone(inside)
                        })
                    }),
                    ..visual.clone()
                })
            })
            .collect()
    }

    /// What is on screen `elapsed_ms` after the start, as tracks that stay there.
    fn held(&self, elapsed_ms: f32, canvas: [f32; 2]) -> Planned {
        let still = Timing {
            delay_ms: 0.0,
            duration_ms: 0.0,
            easing: Easing::Linear,
        };
        let tracks = self
            .on_screen(elapsed_ms, canvas)
            .into_iter()
            .map(|visual| {
                let track = Track::Pair {
                    from: visual.at,
                    to: visual.at,
                    timing: still,
                    source: visual.size,
                };
                (visual, track)
            })
            .collect();
        Planned {
            tracks,
            end_ms: 0.0,
        }
    }
}

/// Whether two items' effects let a Move pair them (ADR-0081, as ADR-0127 amends it):
/// the same kinds in the same order, with settings that are the same or that a Move can
/// blend ([`jstream_render::transition::settings_blend`]).
pub(crate) fn effects_pair(from: &[(String, Settings)], to: &[(String, Settings)]) -> bool {
    from.len() == to.len()
        && from.iter().zip(to).all(|((kind_a, a), (kind_b, b))| {
            kind_a == kind_b && jstream_render::transition::settings_blend(a, b)
        })
}

/// The look of `visual` as `track` draws it `elapsed_ms` in, while it blends its effects'
/// settings (ADR-0127); `None` when it doesn't blend them.
fn blended_look(
    visual: &Visual,
    track: &Track,
    elapsed_ms: f32,
) -> Option<Vec<(String, Settings)>> {
    let from = visual.blend_from.as_ref()?;
    let Track::Pair { timing, .. } = track else {
        return None;
    };
    let k = timing.progress(elapsed_ms);
    Some(
        visual
            .effects
            .iter()
            .zip(from.iter())
            .map(|((kind, to), from)| {
                (
                    kind.clone(),
                    jstream_render::transition::blend_settings(from, to, k),
                )
            })
            .collect(),
    )
}

/// The settings each effect of `visual` draws with `elapsed_ms` into `track`, while it
/// blends them (ADR-0127): `None` when it doesn't.
#[cfg(test)]
pub(crate) fn blended_settings(
    visual: &Visual,
    track: &Track,
    elapsed_ms: f32,
) -> Option<Vec<Settings>> {
    blended_look(visual, track, elapsed_ms)
        .map(|look| look.into_iter().map(|(_, settings)| settings).collect())
}

fn effect(effect: TransitionEffect, unmatched: TransitionEffect) -> Effect {
    match effect.resolve(unmatched) {
        TransitionEffect::Scale => Effect::Scale,
        TransitionEffect::Slide => Effect::Slide,
        TransitionEffect::None => Effect::None,
        _ => Effect::Fade,
    }
}

/// Pairs `from` (what is on screen) with `to` (the new scene's items, bottom first) as
/// `run` says, and returns the tracks in drawing order: the items leaving, then the new
/// scene's.
///
/// With `by_item`, the sides are members of one family and pair by item ID; otherwise
/// by source. An item's progress starts after its delay plus, for the new scene's items,
/// `stagger_ms` × its index; a pair follows the new item's settings. The transition ends
/// when its last item does. Beside the plan: the index in `from` of each track's
/// partner, in the tracks' order, `Some` for a pair.
pub(crate) fn plan(
    from: &[Visual],
    to: &[Visual],
    run: &Run,
    by_item: bool,
) -> (Planned, Vec<Option<usize>>) {
    if let Some(stinger) = &run.stinger {
        return (
            cut_under(from, to, stinger.cut_ms as f32),
            vec![None; from.len() + to.len()],
        );
    }
    let duration_ms = run.duration_ms as f32;
    let timing = |settings: &ItemTransition, delay_ms: f32| Timing {
        delay_ms,
        duration_ms,
        easing: settings.easing.resolve(run.easing),
    };
    let mut used = vec![false; from.len()];
    let mut arriving = Vec::with_capacity(to.len());
    let mut partners: Vec<Option<usize>> = Vec::with_capacity(from.len() + to.len());
    let mut arriving_partners = Vec::with_capacity(to.len());
    for (index, item) in to.iter().enumerate() {
        let delay_ms = item.settings.delay_ms as f32 + index as f32 * run.stagger_ms as f32;
        let partner = (run.pairs && item.settings.glide == Glide::Auto)
            .then(|| {
                from.iter().enumerate().position(|(j, f)| {
                    let same = if by_item {
                        f.item_id == item.item_id
                    } else {
                        f.source_id == item.source_id
                    };
                    !used[j]
                        && same
                        && f.settings.glide == Glide::Auto
                        && effects_pair(&f.effects, &item.effects)
                })
            })
            .flatten();
        let mut arriving_motion = None;
        let mut blend_from = None;
        let track = match partner {
            Some(j) => {
                used[j] = true;
                // Effects whose settings differ blend from the leaving side's (ADR-0127).
                if from[j].effects != item.effects {
                    blend_from = Some(
                        from[j]
                            .effects
                            .iter()
                            .map(|(_, settings)| settings.clone())
                            .collect(),
                    );
                }
                // Both sides' modifiers, blended as it moves (ADR-0099).
                arriving_motion = Some(TrackMotion {
                    from: from[j].motion.to.clone(),
                    to: item.motion.to.clone(),
                });
                Track::Pair {
                    from: from[j].at,
                    to: item.at,
                    timing: timing(&item.settings, delay_ms),
                    source: item.size,
                }
            }
            None => Track::Appear {
                to: item.at,
                effect: effect(item.settings.appear, run.unmatched),
                timing: timing(&item.settings, delay_ms),
            },
        };
        let mut item = item.clone();
        if let Some(motion) = arriving_motion {
            item.motion = motion;
        }
        item.blend_from = blend_from;
        arriving.push((item, track));
        arriving_partners.push(partner);
    }
    let mut tracks: Vec<(Visual, Track)> = from
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|(item, _)| {
            let track = Track::Disappear {
                from: item.at,
                effect: effect(item.settings.disappear, run.unmatched),
                timing: timing(&item.settings, item.settings.delay_ms as f32),
            };
            (item.clone(), track)
        })
        .collect();
    partners.resize(tracks.len(), None);
    tracks.extend(arriving);
    partners.extend(arriving_partners);
    let end_ms = tracks
        .iter()
        .map(|(_, track)| track.end_ms())
        .fold(0.0, f32::max);
    (Planned { tracks, end_ms }, partners)
}

/// A stinger's picture under its overlay: `from` shows until `cut_ms` and `to` from
/// then on, pairing nothing and with no effects.
fn cut_under(from: &[Visual], to: &[Visual], cut_ms: f32) -> Planned {
    let at_cut = Timing {
        delay_ms: cut_ms,
        duration_ms: 0.0,
        easing: Easing::Linear,
    };
    let leaving = from.iter().map(|item| {
        let track = Track::Disappear {
            from: item.at,
            effect: Effect::None,
            timing: at_cut,
        };
        (item.clone(), track)
    });
    let arriving = to.iter().map(|item| {
        let track = Track::Appear {
            to: item.at,
            effect: Effect::None,
            timing: at_cut,
        };
        (item.clone(), track)
    });
    Planned {
        tracks: leaving.chain(arriving).collect(),
        end_ms: cut_ms,
    }
}

/// A stinger's `overlay` (its scene's items, bottom first) as tracks drawn over the
/// switch: each item comes in by its own appear effect after its delay plus
/// `stagger_ms` × its index, and is in by `cut_ms`; it goes by its disappear effect
/// after the cut, by the same delay, and is gone by `duration_ms`.
pub(crate) fn through(
    overlay: &[Visual],
    run: &Run,
    cut_ms: f32,
) -> impl Iterator<Item = (Visual, Track)> {
    let duration_ms = run.duration_ms as f32;
    overlay.iter().enumerate().map(move |(index, item)| {
        let delay_ms = item.settings.delay_ms as f32 + index as f32 * run.stagger_ms as f32;
        let easing = item.settings.easing.resolve(run.easing);
        let enter = Timing {
            delay_ms: delay_ms.min(cut_ms),
            duration_ms: (cut_ms - delay_ms).max(0.0),
            easing,
        };
        let leave_ms = (cut_ms + delay_ms).min(duration_ms);
        let leave = Timing {
            delay_ms: leave_ms,
            duration_ms: duration_ms - leave_ms,
            easing,
        };
        let track = Track::Through {
            at: item.at,
            appear: effect(item.settings.appear, run.unmatched),
            enter,
            disappear: effect(item.settings.disappear, run.unmatched),
            leave,
        };
        (item.clone(), track)
    })
}

/// `track` following `timing` instead of its own: a group's child moves with the group
/// item's progress (ADR-0049 §3).
pub(crate) fn with_timing(track: Track, timing: Timing) -> Track {
    match track {
        Track::Pair {
            from, to, source, ..
        } => Track::Pair {
            from,
            to,
            timing,
            source,
        },
        Track::Appear { to, effect, .. } => Track::Appear { to, effect, timing },
        Track::Disappear { from, effect, .. } => Track::Disappear {
            from,
            effect,
            timing,
        },
        // A stinger's overlay keeps its own: it never moves inside a group.
        through @ Track::Through { .. } => through,
    }
}

/// What `studio.changed` reports for a switch that resolved `preset_id` and runs for
/// `end_ms` (`None` for a cut, reported as 0).
pub(crate) fn started(preset_id: TransitionPresetId, end_ms: Option<f32>) -> TransitionStarted {
    TransitionStarted {
        preset_id,
        duration_ms: end_ms.map_or(0, |ms| ms.max(0.0).ceil() as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::{Planned, Run, Visual};
    use crate::snapshot::SourceRef;
    use jstream_protocol::{
        Easing, Glide, ItemGeometry, ItemId, ItemTransition, Size, SourceId, TransitionEffect,
        TransitionPresetId, Vec2,
    };
    use jstream_render::Crop;
    use jstream_render::transition::{Effect, Endpoint, Track};

    /// A Move between a 3D scene and a 2D one runs as a Fade; between two 3D scenes, or
    /// two 2D ones, it moves (ADR-0094 decision 7).
    #[test]
    fn a_move_between_3d_and_2d_fades() {
        use jstream_graph::Graph;
        use jstream_protocol::{Canvas, Edit, SceneCamera, SceneId, Settings};

        let scene = |id: &str| Edit::SceneCreate {
            scene_id: SceneId::new(id),
            name: id.into(),
            role: None,
        };
        let camera = |id: &str| Edit::SceneSetCamera {
            scene_id: SceneId::new(id),
            camera: Some(SceneCamera::framing(Canvas::DEFAULT)),
        };
        let mut graph = Graph::default();
        graph
            .apply(
                &[
                    scene("flat"),
                    scene("flat2"),
                    scene("set"),
                    scene("set2"),
                    camera("set"),
                    camera("set2"),
                    Edit::TransitionCreate {
                        preset_id: TransitionPresetId::new("glide"),
                        name: "Glide".into(),
                        kind: "move".into(),
                        duration_ms: 600,
                        easing: Easing::EaseOut,
                        settings: Settings::new(),
                    },
                ],
                None,
            )
            .expect("edits");
        let pairs = |from: &str, to: &str| {
            let named = TransitionPresetId::new("glide");
            super::resolve(
                &graph,
                Some(&SceneId::new(from)),
                &SceneId::new(to),
                Some(&named),
            )
            .expect("resolves")
            .1
            .expect("runs")
            .pairs
        };
        assert!(pairs("set", "set2"));
        assert!(pairs("flat", "flat2"));
        assert!(!pairs("set", "flat"));
        assert!(!pairs("flat", "set"));
    }

    fn plan(from: &[Visual], to: &[Visual], run: &Run, by_item: bool) -> Planned {
        super::plan(from, to, run, by_item).0
    }

    fn visual(source: &str, x: f32) -> Visual {
        item(&format!("{source}_{x}"), source, x)
    }

    /// `item_id` placing `source` at `x`.
    fn item(item_id: &str, source: &str, x: f32) -> Visual {
        Visual {
            image: SourceRef::Frame(SourceId::new(source)),
            item_id: ItemId::new(item_id),
            source_id: SourceId::new(source),
            at: Endpoint {
                geometry: ItemGeometry::try_new(
                    Vec2::new(x, 100.0).expect("finite"),
                    Size::try_new(10.0, 10.0).expect("positive"),
                )
                .expect("valid"),
                crop: Crop::NONE,
                opacity: 1.0,
                blend: jstream_protocol::BlendMode::Normal,
            },
            size: None,
            settings: ItemTransition::default(),
            effects: Vec::new(),
            blend_from: None,
            inside: None,
            seen: super::Seen::AsPlanned,
            motion: super::TrackMotion::same(ItemId::new(item_id), std::sync::Arc::from([])),
        }
    }

    fn run(pairs: bool) -> Run {
        Run {
            preset_id: TransitionPresetId::new("p"),
            pairs,
            duration_ms: 500,
            easing: Easing::Linear,
            unmatched: TransitionEffect::Scale,
            stagger_ms: 100,
            stinger: None,
        }
    }

    fn kinds(planned: &Planned) -> Vec<(String, &'static str)> {
        planned
            .tracks
            .iter()
            .map(|(visual, track)| {
                let kind = match track {
                    Track::Pair { .. } => "pair",
                    Track::Appear { .. } => "appear",
                    Track::Disappear { .. } => "disappear",
                    Track::Through { .. } => "through",
                };
                (visual.source_id.to_string(), kind)
            })
            .collect()
    }

    /// Two placements of a source pair only when their effects are the same filters in
    /// order (ADR-0065 §6, ADR-0081); settings that differ blend from the leaving side's
    /// (ADR-0127). A different list, or a reference that differs, takes each one's
    /// unmatched effect.
    #[test]
    fn items_pair_when_their_effects_are_the_same_kinds_and_blend_their_settings() {
        let framed = |x: f32, strength: serde_json::Value| Visual {
            effects: vec![(
                "vignette".into(),
                [("strength", strength)].into_iter().collect(),
            )],
            ..visual("cam", x)
        };
        use serde_json::json;
        let same = plan(
            &[framed(0.0, json!(0.5))],
            &[framed(10.0, json!(0.5))],
            &run(true),
            false,
        );
        assert_eq!(kinds(&same), [("cam".into(), "pair")]);
        assert_eq!(same.tracks[0].0.blend_from, None, "nothing to blend");
        let other = plan(
            &[framed(0.0, json!(0.5))],
            &[framed(10.0, json!(0.8))],
            &run(true),
            false,
        );
        assert_eq!(kinds(&other), [("cam".into(), "pair")]);
        let (paired, track) = &other.tracks[0];
        let from = paired.blend_from.as_deref().expect("it blends");
        assert_eq!(from[0].get("strength"), Some(&json!(0.5)));
        // Linear over 500 ms after the first item's 0 ms stagger: halfway at 250.
        let mid = super::blended_settings(paired, track, 250.0).expect("blends");
        let strength = mid[0].get("strength").and_then(|v| v.as_f64());
        assert!(
            strength.is_some_and(|v| (v - 0.65).abs() < 1e-6),
            "{strength:?}"
        );
        let scene = |id: &str| json!({"type": "scene", "scene_id": id});
        let masked = plan(
            &[framed(0.0, scene("a"))],
            &[framed(10.0, scene("b"))],
            &run(true),
            false,
        );
        assert_eq!(
            kinds(&masked),
            [("cam".into(), "disappear"), ("cam".into(), "appear")]
        );
        let bare = plan(
            &[visual("cam", 0.0)],
            &[framed(10.0, json!(0.5))],
            &run(true),
            false,
        );
        assert_eq!(
            kinds(&bare),
            [("cam".into(), "disappear"), ("cam".into(), "appear")]
        );
    }

    #[test]
    fn move_pairs_the_kth_placement_of_a_source_with_the_kth() {
        let from = [visual("cam", 0.0), visual("logo", 1.0), visual("cam", 2.0)];
        let to = [visual("cam", 10.0), visual("bg", 11.0)];
        let planned = plan(&from, &to, &run(true), false);
        assert_eq!(
            kinds(&planned),
            [
                ("logo".into(), "disappear"),
                ("cam".into(), "disappear"),
                ("cam".into(), "pair"),
                ("bg".into(), "appear"),
            ]
        );
        // The first `cam` paired: the second is the one leaving.
        let Track::Disappear { from: leaving, .. } = planned.tracks[1].1 else {
            panic!("not leaving");
        };
        assert_eq!(leaving.geometry.center.x.get(), 2.0);
        // `bg` is index 1: 100 ms of stagger, then 500 ms.
        assert_eq!(planned.end_ms, 600.0);
        let Track::Appear { effect, .. } = planned.tracks[3].1 else {
            panic!("not appearing");
        };
        assert_eq!(effect, Effect::Scale, "the preset's unmatched effect");
    }

    #[test]
    fn fade_and_glide_never_pair_nothing() {
        let from = [visual("cam", 0.0)];
        let to = [visual("cam", 10.0)];
        assert_eq!(
            kinds(&plan(&from, &to, &run(false), false)),
            [("cam".into(), "disappear"), ("cam".into(), "appear")]
        );
        let mut never = visual("cam", 10.0);
        never.settings.glide = Glide::Never;
        assert_eq!(
            kinds(&plan(&from, &[never], &run(true), false)),
            [("cam".into(), "disappear"), ("cam".into(), "appear")]
        );
    }

    #[test]
    fn retargeting_starts_from_what_is_on_screen() {
        let from = [visual("cam", 0.0)];
        let to = [visual("cam", 100.0)];
        let mut linear = run(true);
        linear.stagger_ms = 0;
        let planned = plan(&from, &to, &linear, false);
        let on_screen = planned.on_screen(250.0, [1920.0, 1080.0]);
        assert_eq!(on_screen.len(), 1);
        assert_eq!(on_screen[0].at.geometry.center.x.get(), 50.0);
        // Going back pairs the half-way item with the original one.
        let back = plan(&on_screen, &from, &linear, false);
        let Track::Pair { from: start, .. } = back.tracks[0].1 else {
            panic!("not paired");
        };
        assert_eq!(start.geometry.center.x.get(), 50.0);
    }

    #[test]
    fn within_a_family_move_pairs_each_item_with_itself() {
        // `cam` is placed twice; the keyframe swaps the two placements' positions.
        let from = [item("a", "cam", 0.0), item("b", "cam", 50.0)];
        let to = [item("b", "cam", 0.0), item("a", "cam", 50.0)];
        let planned = plan(&from, &to, &run(true), true);
        let moves: Vec<(String, f32, f32)> = planned
            .tracks
            .iter()
            .map(|(visual, track)| {
                let Track::Pair { from, to, .. } = track else {
                    panic!("not paired: {track:?}");
                };
                let x = |e: &Endpoint| e.geometry.center.x.get();
                (visual.item_id.to_string(), x(from), x(to))
            })
            .collect();
        // By item, each placement moves to where the keyframe puts it; by source the
        // k-th would pair with the k-th and neither would move.
        assert_eq!(moves, [("b".into(), 50.0, 0.0), ("a".into(), 0.0, 50.0)]);
        let by_source = plan(&from, &to, &run(true), false);
        let Track::Pair { from: start, .. } = by_source.tracks[0].1 else {
            panic!("not paired");
        };
        assert_eq!(start.geometry.center.x.get(), 0.0);
        // An item hidden on one side appears or disappears.
        let planned = plan(&from[..1], &to, &run(true), true);
        assert_eq!(
            kinds(&planned),
            [("cam".into(), "appear"), ("cam".into(), "pair")]
        );
    }
}
