//! Audio that follows the program: each audio source's activity gain (ADR-0028,
//! docs/adr/0028-audio-model-v2.md, and ARCHITECTURE › Data model, "Shown and on
//! program").
//!
//! A **root** is a scene the Program mix shows, with a weight: the program scene at 1,
//! as only a cut exists so far (ADR-0030; a transition's two scenes and its audio curve
//! come with T1). A nested scene source whose audio is on and follows `always` is an
//! extra root of weight 1. A source is **reachable** from a scene through its visible
//! items, following nested scene sources whose audio is on, to any depth. A
//! `when_shown` source's activity gain is `min(1, Σ w(r))` over the roots `r` that
//! reach it; an `always` source's is 1. The gain is runtime state, never an edit: the
//! routing (`crate::audio`) hands it to each mixer input, which ramps to it over one
//! block.

use jstream_graph::State;
use jstream_protocol::{AudioFollow, SceneId, SceneRole, SourceId, SourceKind};
use std::collections::{BTreeMap, BTreeSet};

/// The activity gain of every audio source in `state` while `program` is on the
/// Program mix. Sources without audio are left out.
pub(crate) fn activity_gains(state: &State, program: Option<&SceneId>) -> BTreeMap<SourceId, f32> {
    let roots: Vec<(&SceneId, f32)> = program.map(|scene| (scene, 1.0)).into_iter().collect();
    weighted_gains(state, &roots)
}

/// [`activity_gains`] for the weighted `roots` the Program mix shows (their weights sum
/// to 1 during a transition).
fn weighted_gains(state: &State, roots: &[(&SceneId, f32)]) -> BTreeMap<SourceId, f32> {
    let always_roots = state.sources().filter_map(|source| match source.kind() {
        SourceKind::Scene { scene_id }
            if source
                .audio()
                .is_some_and(|audio| audio.enabled && audio.follow == AudioFollow::Always) =>
        {
            Some((scene_id, 1.0))
        }
        _ => None,
    });
    let mut shown: BTreeMap<&SourceId, f32> = BTreeMap::new();
    for (root, weight) in roots.iter().copied().chain(always_roots) {
        // Each root counts once per source, however many paths reach it.
        for source in reachable(state, root) {
            *shown.entry(source).or_default() += weight;
        }
    }
    state
        .sources()
        .filter_map(|source| {
            let audio = source.audio()?;
            let gain = if audio.follow == AudioFollow::WhenShown {
                shown.get(source.id()).copied().unwrap_or(0.0).min(1.0)
            } else {
                1.0
            };
            Some((source.id().clone(), gain))
        })
        .collect()
}

/// The sources `root` reaches: those its visible items place, and through each placed
/// nested scene source whose audio is on, what that scene reaches. A keyframe scene's
/// overrides apply to its parent's items and to the children of the groups in its tree
/// (ADR-0024 §2), so a child it hides is not reached. The scene graph has no cycles;
/// each scene is still walked once per keyframe it is reached through.
fn reachable<'a>(state: &'a State, root: &SceneId) -> BTreeSet<&'a SourceId> {
    let mut reached = BTreeSet::new();
    let mut walked = BTreeSet::new();
    let keyframe = state
        .scene(root)
        .filter(|scene| scene.variant_of().is_some());
    let mut pending = vec![(root.clone(), keyframe)];
    while let Some((scene_id, keyframe)) = pending.pop() {
        if !walked.insert((scene_id.clone(), keyframe.map(|k| k.id().clone()))) {
            continue;
        }
        let Some(scene) = state.scene(&scene_id) else {
            continue;
        };
        let items = match keyframe {
            Some(keyframe) if scene.variant_of().is_none() => State::items_through(scene, keyframe),
            _ => state.shown_items(scene),
        };
        for item in items.into_iter().filter(|item| item.visible()) {
            let Some(source) = state.source(item.source_id()) else {
                continue;
            };
            reached.insert(source.id());
            if let SourceKind::Scene { scene_id } = source.kind()
                && source.audio().is_none_or(|audio| audio.enabled)
            {
                // Only a group stays in the keyframe's tree.
                let group = state
                    .scene(scene_id)
                    .is_some_and(|inner| inner.role() == SceneRole::Group);
                pending.push((scene_id.clone(), keyframe.filter(|_| group)));
            }
        }
    }
    reached
}

#[cfg(test)]
mod tests {
    use super::{activity_gains, weighted_gains};
    use jstream_graph::Graph;
    use jstream_protocol::{
        AudioFollow, Edit, ItemId, KindCategory, KindDescriptor, SceneId, Settings, SourceId,
        SourceKind,
    };

    fn apply(graph: &mut Graph, edits: Vec<Edit>) {
        graph.apply(&edits, None).expect("the edits apply");
    }

    fn scene(id: &str) -> Edit {
        Edit::SceneCreate {
            scene_id: SceneId::new(id),
            name: id.to_uppercase(),
            role: None,
        }
    }

    fn source(id: &str, kind: &str) -> Edit {
        Edit::SourceCreate {
            source_id: SourceId::new(id),
            name: id.to_uppercase(),
            kind: SourceKind::Builtin { kind: kind.into() },
            settings: Settings::new(),
            duplicated_from: None,
        }
    }

    fn nested(id: &str, scene_id: &str) -> Edit {
        Edit::SourceCreate {
            source_id: SourceId::new(id),
            name: id.to_uppercase(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new(scene_id),
            },
            settings: Settings::new(),
            duplicated_from: None,
        }
    }

    fn item(scene_id: &str, item_id: &str, source_id: &str) -> Edit {
        Edit::ItemAdd {
            scene_id: SceneId::new(scene_id),
            item_id: ItemId::new(item_id),
            source_id: SourceId::new(source_id),
            transform: None,
            geometry: None,
            opacity: None,
            blend: None,
        }
    }

    fn hide(item_id: &str) -> Edit {
        Edit::ItemSetVisible {
            item_id: ItemId::new(item_id),
            scene_id: None,
            visible: false,
        }
    }

    fn set_audio(source_id: &str, enabled: Option<bool>, follow: Option<AudioFollow>) -> Edit {
        Edit::SourceSetAudio {
            source_id: SourceId::new(source_id),
            enabled,
            follow,
            gain_db: None,
            muted: None,
        }
    }

    fn kind(id: &str, video: bool) -> KindDescriptor {
        KindDescriptor {
            kind_id: id.into(),
            category: KindCategory::Source,
            name: id.into(),
            version: 1,
            video,
            audio: true,
            max_outset_px: None,
            params: vec![],
        }
    }

    /// Scenes `a` and `b`. `a` shows the clip `media` (follows `when_shown`) and the
    /// nested scene `inner` (scene `c`, holding the clip `nested_media`). The `mic`
    /// (follows `always`) is placed nowhere. `b` shows `b_media`.
    fn graph() -> Graph {
        let mut graph = Graph::new().with_kinds(vec![kind("clip", true), kind("tone", false)]);
        apply(
            &mut graph,
            vec![
                scene("a"),
                scene("b"),
                scene("c"),
                source("media", "clip"),
                source("nested_media", "clip"),
                source("b_media", "clip"),
                source("mic", "tone"),
                nested("inner", "c"),
                item("a", "a1", "media"),
                item("a", "a2", "inner"),
                item("c", "c1", "nested_media"),
                item("b", "b1", "b_media"),
            ],
        );
        graph
    }

    fn gain(gains: &std::collections::BTreeMap<SourceId, f32>, id: &str) -> f32 {
        gains[&SourceId::new(id)]
    }

    /// A keyframe scene that hides a group's child stops what the child shows from
    /// being heard there, and the parent still hears it (#846).
    #[test]
    fn a_keyframe_hiding_a_groups_child_silences_it() {
        let mut graph = graph();
        apply(
            &mut graph,
            vec![
                Edit::SceneSetRole {
                    scene_id: SceneId::new("c"),
                    role: jstream_protocol::SceneRole::Group,
                },
                Edit::SceneCreateVariant {
                    scene_id: SceneId::new("k"),
                    variant_of: SceneId::new("a"),
                    name: "K".into(),
                },
                Edit::ItemSetVisible {
                    item_id: ItemId::new("c1"),
                    scene_id: Some(SceneId::new("k")),
                    visible: false,
                },
            ],
        );
        let k = SceneId::new("k");
        let gains = activity_gains(graph.state(), Some(&k));
        assert_eq!(gain(&gains, "media"), 1.0);
        assert_eq!(gain(&gains, "nested_media"), 0.0, "hidden in the keyframe");
        let a = SceneId::new("a");
        let gains = activity_gains(graph.state(), Some(&a));
        assert_eq!(gain(&gains, "nested_media"), 1.0);
    }

    #[test]
    fn program_sources_are_heard_and_others_are_not() {
        let graph = graph();
        let a = SceneId::new("a");
        let gains = activity_gains(graph.state(), Some(&a));
        assert_eq!(gain(&gains, "media"), 1.0);
        assert_eq!(gain(&gains, "nested_media"), 1.0, "nesting is followed");
        assert_eq!(gain(&gains, "b_media"), 0.0);
        assert_eq!(gain(&gains, "mic"), 1.0, "always is always heard");
        let b = SceneId::new("b");
        let gains = activity_gains(graph.state(), Some(&b));
        assert_eq!(gain(&gains, "media"), 0.0);
        assert_eq!(gain(&gains, "nested_media"), 0.0);
        assert_eq!(gain(&gains, "b_media"), 1.0);
        let gains = activity_gains(graph.state(), None);
        assert_eq!(
            gain(&gains, "media"),
            0.0,
            "nothing is shown without program"
        );
        assert_eq!(gain(&gains, "mic"), 1.0);
    }

    #[test]
    fn hidden_items_and_nested_scenes_with_audio_off_stop_reachability() {
        let mut graph = graph();
        let a = SceneId::new("a");
        apply(&mut graph, vec![set_audio("inner", Some(false), None)]);
        let gains = activity_gains(graph.state(), Some(&a));
        assert_eq!(gain(&gains, "nested_media"), 0.0);
        // Another path still counts: `media` placed directly in `c` too.
        apply(
            &mut graph,
            vec![
                set_audio("inner", Some(true), None),
                item("c", "c2", "media"),
                hide("a1"),
            ],
        );
        let gains = activity_gains(graph.state(), Some(&a));
        assert_eq!(gain(&gains, "media"), 1.0, "reached through inner");
        apply(&mut graph, vec![hide("a2")]);
        let gains = activity_gains(graph.state(), Some(&a));
        assert_eq!(gain(&gains, "media"), 0.0);
        assert_eq!(gain(&gains, "nested_media"), 0.0);
    }

    #[test]
    fn a_nested_scene_that_follows_always_is_an_extra_root() {
        let mut graph = graph();
        let b = SceneId::new("b");
        apply(
            &mut graph,
            vec![set_audio("inner", None, Some(AudioFollow::Always))],
        );
        let gains = activity_gains(graph.state(), Some(&b));
        assert_eq!(
            gain(&gains, "nested_media"),
            1.0,
            "off program, still heard"
        );
        assert_eq!(gain(&gains, "media"), 0.0, "only what inner reaches");
        assert_eq!(gain(&gains, "inner"), 1.0);
        // On program as well, it still sums to no more than 1.
        let a = SceneId::new("a");
        let gains = activity_gains(graph.state(), Some(&a));
        assert_eq!(gain(&gains, "nested_media"), 1.0);
        // With its audio off it is no root.
        apply(&mut graph, vec![set_audio("inner", Some(false), None)]);
        let gains = activity_gains(graph.state(), Some(&b));
        assert_eq!(gain(&gains, "nested_media"), 0.0);
    }

    /// The weighted form T1's transitions will use: a source in both scenes stays at
    /// full level, one in either scene follows that scene's weight.
    #[test]
    fn weights_add_up_to_at_most_one() {
        let mut graph = graph();
        apply(&mut graph, vec![item("b", "b2", "media")]);
        let (a, b) = (SceneId::new("a"), SceneId::new("b"));
        let gains = weighted_gains(graph.state(), &[(&a, 0.25), (&b, 0.75)]);
        assert_eq!(gain(&gains, "media"), 1.0);
        assert_eq!(gain(&gains, "nested_media"), 0.25);
        assert_eq!(gain(&gains, "b_media"), 0.75);
    }
}
