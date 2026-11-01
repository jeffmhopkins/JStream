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
    Easing, EntityKind, FADE, Glide, ItemId, ItemTransition, MOVE, ProtocolError, SceneId,
    Settings, SourceId, TransitionEffect, TransitionPresetId, TransitionStarted,
};
use jstream_render::transition::{Effect, Endpoint, Timing, Track};

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
        MOVE => true,
        FADE => false,
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
    let run = Run {
        preset_id: id.clone(),
        pairs,
        duration_ms: preset.duration_ms(),
        easing: preset.easing(),
        unmatched: TransitionEffect::from_setting(settings.get("unmatched")),
        stagger_ms,
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
    /// pair only when theirs are the same (§6).
    pub(crate) effects: Vec<(String, Settings)>,
    /// For a group that moves as one item within a family, its children's tracks
    /// inside its layer (ADR-0049 §3); `None` for everything else.
    pub(crate) inside: Option<Arc<Inside>>,
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
}

impl Running {
    /// Milliseconds from its start to `at`.
    pub(crate) fn elapsed_ms(&self, at: MediaTime) -> f32 {
        elapsed_ms(self.start, at)
    }
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
        self.tracks
            .iter()
            .filter_map(|(visual, track)| {
                track.sample(elapsed_ms, canvas).map(|at| Visual {
                    at,
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
                    !used[j] && same && f.settings.glide == Glide::Auto && f.effects == item.effects
                })
            })
            .flatten();
        let track = match partner {
            Some(j) => {
                used[j] = true;
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
        arriving.push((item.clone(), track));
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
            inside: None,
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
                };
                (visual.source_id.to_string(), kind)
            })
            .collect()
    }

    /// Two placements of a source pair only when their effects are the same filters in
    /// order with the same settings (ADR-0065 §6); otherwise each takes its unmatched
    /// effect.
    #[test]
    fn items_pair_only_when_their_effects_match() {
        let framed = |x: f32, strength: f64| Visual {
            effects: vec![(
                "vignette".into(),
                [("strength", serde_json::json!(strength))]
                    .into_iter()
                    .collect(),
            )],
            ..visual("cam", x)
        };
        let same = plan(&[framed(0.0, 0.5)], &[framed(10.0, 0.5)], &run(true), false);
        assert_eq!(kinds(&same), [("cam".into(), "pair")]);
        let other = plan(&[framed(0.0, 0.5)], &[framed(10.0, 0.8)], &run(true), false);
        assert_eq!(
            kinds(&other),
            [("cam".into(), "disappear"), ("cam".into(), "appear")]
        );
        let bare = plan(
            &[visual("cam", 0.0)],
            &[framed(10.0, 0.5)],
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
