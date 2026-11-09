//! Studio state: which scene is on program and which on preview (ADR-0030), and the
//! transition running on program (ADR-0029).
//!
//! Program and preview are runtime state, beside the outputs' state, not collection
//! state: `scene.switch` and `studio.set_preview` change them without a new `rev`, and
//! undo never touches them. Each always names a scene the collection has, whenever the
//! collection has one. The collection's first scene goes on program and on preview when
//! it is created, and when a batch deletes either scene, it moves to the first scene
//! left in display order. Reordering scenes never moves them. Preview is studio mode's
//! next scene: it changes nothing an output shows unless the output's mix is the preview
//! mix.
//!
//! A switch puts its scene on program at once; when its preset runs a transition, the
//! studio keeps it as [`Running`] until it ends, and the program mix draws it.
//!
//! The studio also names the scenes kept warm beside the program (ADR-0117 §7): the
//! Preview scene, then the scenes kept warm by their `keep_warm` option, then those a
//! hotkey or a trigger switches to, then the scenes most recently on program, at most
//! the warm budget (`studio.set_warm`, [`DEFAULT_WARM_SCENES`] until it is set). The Preview scene is
//! warm even past the budget, so a budget of 0 keeps only it. A running transition's
//! destination is the program scene itself. The budget is an application setting: a
//! new collection's studio keeps it ([`Studio::fresh`]). So is its limit on the GPU bytes
//! the warm scenes hold (ADR-0150), which the engine applies once it has planned them.

use jstream_graph::State;
use jstream_protocol::{
    Action, DEFAULT_WARM_SCENES, EntityKind, MAX_WARM_SCENES, ProtocolError, SceneId,
};

use crate::transition::Running;

/// The most scenes the studio remembers as recently on program: as many as the largest
/// budget can keep warm.
const RECENT: usize = MAX_WARM_SCENES as usize;

/// The studio state the engine keeps under its collection lock.
#[derive(Debug)]
pub(crate) struct Studio {
    program: Option<SceneId>,
    preview: Option<SceneId>,
    running: Option<Running>,
    /// The scenes on program before the current one, the most recent first, never the
    /// program scene, at most [`RECENT`].
    recent: Vec<SceneId>,
    /// How many scenes besides the program are kept warm, at most [`MAX_WARM_SCENES`].
    budget: usize,
    /// The limit on the warm scenes' estimated GPU memory in MiB, 0 for none
    /// (ADR-0150).
    gpu_mib: u32,
}

impl Default for Studio {
    fn default() -> Self {
        Studio {
            program: None,
            preview: None,
            running: None,
            recent: Vec::new(),
            budget: DEFAULT_WARM_SCENES as usize,
            gpu_mib: 0,
        }
    }
}

impl Studio {
    /// The studio for a newly opened collection: nothing on program or preview yet, and
    /// this studio's warm budget, which belongs to the application.
    pub(crate) fn fresh(&self) -> Studio {
        Studio {
            budget: self.budget,
            gpu_mib: self.gpu_mib,
            ..Studio::default()
        }
    }

    /// How many scenes besides the program are kept warm.
    pub(crate) fn budget(&self) -> usize {
        self.budget
    }

    /// Sets the warm budget, which the caller checked is at most [`MAX_WARM_SCENES`].
    /// `true` when it changed.
    pub(crate) fn set_budget(&mut self, budget: usize) -> bool {
        let budget = budget.min(RECENT);
        let changed = self.budget != budget;
        self.budget = budget;
        changed
    }

    /// The limit on the warm scenes' estimated GPU memory in MiB, 0 for none.
    pub(crate) fn gpu_mib(&self) -> u32 {
        self.gpu_mib
    }

    /// The same limit in bytes, `None` for none.
    pub(crate) fn gpu_limit(&self) -> Option<u64> {
        (self.gpu_mib > 0).then(|| u64::from(self.gpu_mib) << 20)
    }

    /// Sets the GPU-byte limit, which the caller checked is at most
    /// [`MAX_WARM_GPU_MIB`](jstream_protocol::MAX_WARM_GPU_MIB). `true` when it changed.
    pub(crate) fn set_gpu_mib(&mut self, gpu_mib: u32) -> bool {
        let changed = self.gpu_mib != gpu_mib;
        self.gpu_mib = gpu_mib;
        changed
    }

    /// The scene on program, or `None` while the collection has no scene.
    pub(crate) fn program(&self) -> Option<&SceneId> {
        self.program.as_ref()
    }

    /// The scene on preview, or `None` while the collection has no scene.
    pub(crate) fn preview(&self) -> Option<&SceneId> {
        self.preview.as_ref()
    }

    /// The transition running toward the program scene, if one is.
    pub(crate) fn running(&self) -> Option<&Running> {
        self.running.as_ref()
    }

    /// Puts `scene_id`, which the caller checked, on program, with `running` as the
    /// transition toward it, or none for a cut.
    pub(crate) fn switch(&mut self, scene_id: SceneId, running: Option<Running>) {
        if let Some(left) = self.program.take().filter(|left| *left != scene_id) {
            self.recent.retain(|id| *id != left);
            self.recent.insert(0, left);
        }
        self.recent.retain(|id| *id != scene_id);
        self.recent.truncate(RECENT);
        self.program = Some(scene_id);
        self.running = running;
    }

    /// The scenes kept warm beside the program, in priority order (ADR-0117 §7): the
    /// Preview scene, even past the budget; then the scenes `collection` keeps warm
    /// (`keep_warm`), in display order; then the scenes its hotkeys and its trigger
    /// bindings switch to; then the scenes most recently on program. At most the budget
    /// in all, the Preview scene counted, never the program scene, each once.
    pub(crate) fn warm(&self, collection: &State) -> Vec<SceneId> {
        let mut warm: Vec<SceneId> = Vec::with_capacity(self.budget.max(1));
        let program = self.program.as_ref();
        if let Some(preview) = self.preview.as_ref().filter(|id| Some(*id) != program) {
            warm.push(preview.clone());
        }
        let pinned = collection
            .scenes()
            .iter()
            .filter(|scene| scene.options().keep_warm)
            .map(|scene| scene.id());
        let bound = collection
            .hotkeys()
            .map(|(_, action)| action)
            .chain(collection.bindings().map(|(_, action)| action))
            .filter_map(|action| match action {
                Action::SceneSwitch { scene_id, .. } => Some(scene_id),
                _ => None,
            })
            .filter(|id| collection.scene(id).is_some());
        for id in pinned.chain(bound).chain(&self.recent) {
            if warm.len() >= self.budget {
                break;
            }
            if Some(id) != program && !warm.contains(id) {
                warm.push(id.clone());
            }
        }
        warm
    }

    /// Lets the running transition go once it has ended.
    pub(crate) fn finish(&mut self) {
        self.running = None;
    }

    /// Puts `scene_id` on preview. `Ok(true)` when preview changed, `Ok(false)` when the
    /// scene was already on preview, and `not_found` when the collection has no such
    /// scene.
    pub(crate) fn set_preview(
        &mut self,
        collection: &State,
        scene_id: &SceneId,
    ) -> Result<bool, ProtocolError> {
        exists(collection, scene_id)?;
        if self.preview.as_ref() == Some(scene_id) {
            return Ok(false);
        }
        self.preview = Some(scene_id.clone());
        Ok(true)
    }

    /// Follows the collection after it changed. A program or preview scene the
    /// collection no longer has gives way to its first scene, and an empty one takes the
    /// first scene once there is one. A transition toward a program scene that moved is
    /// dropped: the new one shows at once. A recent scene the collection no longer has
    /// is forgotten. `true` when program or preview changed.
    pub(crate) fn reconcile(&mut self, collection: &State) -> bool {
        self.recent.retain(|id| collection.scene(id).is_some());
        let first = collection.scenes().first().map(|scene| scene.id().clone());
        let mut changed = false;
        for slot in [&mut self.program, &mut self.preview] {
            let kept = slot
                .as_ref()
                .is_some_and(|id| collection.scene(id).is_some());
            if !kept && *slot != first {
                *slot = first.clone();
                changed = true;
            }
        }
        if self
            .running
            .as_ref()
            .is_some_and(|running| Some(&running.to) != self.program.as_ref())
        {
            self.running = None;
        }
        changed
    }
}

/// `not_found` unless the collection has scene `scene_id`.
pub(crate) fn exists(collection: &State, scene_id: &SceneId) -> Result<(), ProtocolError> {
    match collection.scene(scene_id) {
        Some(_) => Ok(()),
        None => Err(ProtocolError::NotFound {
            entity: EntityKind::Scene,
            id: scene_id.as_str().to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::Studio;
    use jstream_graph::Graph;
    use jstream_protocol::{Edit, EntityKind, ProtocolError, SceneId};

    fn scene(id: &str) -> SceneId {
        SceneId::new(id)
    }

    fn apply(graph: &mut Graph, edits: Vec<Edit>) {
        graph.apply(&edits, None).expect("the edits apply");
    }

    fn create(id: &str) -> Edit {
        Edit::SceneCreate {
            scene_id: scene(id),
            name: id.to_uppercase(),
            role: None,
        }
    }

    #[test]
    fn the_first_scene_goes_on_program_and_reordering_keeps_it() {
        let mut graph = Graph::new();
        let mut studio = Studio::default();
        assert!(!studio.reconcile(graph.state()));
        assert_eq!(studio.program(), None);

        apply(&mut graph, vec![create("a"), create("b")]);
        assert!(studio.reconcile(graph.state()));
        assert_eq!(studio.program(), Some(&scene("a")));
        assert_eq!(studio.preview(), Some(&scene("a")));

        apply(
            &mut graph,
            vec![Edit::SceneReorder {
                scene_id: scene("b"),
                index: 0,
            }],
        );
        assert!(!studio.reconcile(graph.state()));
        assert_eq!(studio.program(), Some(&scene("a")));
    }

    #[test]
    fn preview_names_an_existing_scene_and_is_idempotent() {
        let mut graph = Graph::new();
        let mut studio = Studio::default();
        apply(&mut graph, vec![create("a"), create("b")]);
        studio.reconcile(graph.state());
        assert_eq!(studio.preview(), Some(&scene("a")));

        assert_eq!(studio.set_preview(graph.state(), &scene("b")), Ok(true));
        assert_eq!(studio.set_preview(graph.state(), &scene("b")), Ok(false));
        assert_eq!(studio.preview(), Some(&scene("b")));
        assert_eq!(
            studio.set_preview(graph.state(), &scene("ghost")),
            Err(ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: "ghost".into(),
            })
        );
        assert_eq!(studio.preview(), Some(&scene("b")));

        studio.switch(scene("b"), None);
        assert_eq!(studio.program(), Some(&scene("b")));
        assert_eq!(
            studio.preview(),
            Some(&scene("b")),
            "a switch keeps preview"
        );
    }

    #[test]
    fn deleting_the_program_scene_moves_program_to_the_first_scene() {
        let mut graph = Graph::new();
        let mut studio = Studio::default();
        apply(&mut graph, vec![create("a"), create("b"), create("c")]);
        studio.reconcile(graph.state());
        studio.switch(scene("c"), None);
        studio
            .set_preview(graph.state(), &scene("c"))
            .expect("c exists");

        apply(
            &mut graph,
            vec![Edit::SceneDelete {
                scene_id: scene("c"),
            }],
        );
        assert!(studio.reconcile(graph.state()));
        assert_eq!(studio.program(), Some(&scene("a")));

        apply(
            &mut graph,
            vec![
                Edit::SceneDelete {
                    scene_id: scene("a"),
                },
                Edit::SceneDelete {
                    scene_id: scene("b"),
                },
            ],
        );
        assert!(studio.reconcile(graph.state()));
        assert_eq!(studio.program(), None);
    }

    #[test]
    fn warm_scenes_are_preview_then_the_most_recent_without_the_program() {
        let mut graph = Graph::new();
        let mut studio = Studio::default();
        apply(
            &mut graph,
            ["a", "b", "c", "d", "e"].into_iter().map(create).collect(),
        );
        studio.reconcile(graph.state());
        assert_eq!(
            studio.warm(graph.state()),
            Vec::<SceneId>::new(),
            "preview is the program"
        );

        studio.switch(scene("b"), None);
        assert_eq!(
            studio.warm(graph.state()),
            vec![scene("a")],
            "a is preview and was shown"
        );
        studio.switch(scene("c"), None);
        studio.switch(scene("b"), None);
        assert_eq!(studio.warm(graph.state()), vec![scene("a"), scene("c")]);
        studio.switch(scene("b"), None);
        assert_eq!(
            studio.warm(graph.state()),
            vec![scene("a"), scene("c")],
            "switching to the program again changes nothing"
        );

        studio
            .set_preview(graph.state(), &scene("e"))
            .expect("e exists");
        studio.switch(scene("d"), None);
        assert_eq!(
            studio.warm(graph.state()),
            vec![scene("e"), scene("b"), scene("c")],
            "preview first, then the most recent, capped"
        );

        apply(
            &mut graph,
            vec![Edit::SceneDelete {
                scene_id: scene("b"),
            }],
        );
        studio.reconcile(graph.state());
        assert_eq!(
            studio.warm(graph.state()),
            vec![scene("e"), scene("c"), scene("a")],
            "a deleted scene is forgotten"
        );
    }

    #[test]
    fn the_budget_bounds_the_recent_scenes_but_never_drops_preview() {
        let mut graph = Graph::new();
        let mut studio = Studio::default();
        apply(
            &mut graph,
            ["a", "b", "c", "d", "e", "f"]
                .into_iter()
                .map(create)
                .collect(),
        );
        studio.reconcile(graph.state());
        for id in ["b", "c", "d", "e", "f"] {
            studio.switch(scene(id), None);
        }
        studio
            .set_preview(graph.state(), &scene("a"))
            .expect("a exists");
        assert_eq!(
            studio.warm(graph.state()),
            vec![scene("a"), scene("e"), scene("d")],
            "the default budget is 3"
        );

        assert!(studio.set_budget(0));
        assert_eq!(
            studio.warm(graph.state()),
            vec![scene("a")],
            "preview stays warm at 0"
        );
        assert!(!studio.set_budget(0), "the same budget changes nothing");

        assert!(studio.set_budget(5));
        assert_eq!(
            studio.warm(graph.state()),
            vec![scene("a"), scene("e"), scene("d"), scene("c"), scene("b")],
            "a larger budget brings back scenes shown before it was set"
        );

        let fresh = studio.fresh();
        assert_eq!(fresh.budget(), 5, "a new collection keeps the budget");
        assert_eq!(fresh.program(), None);
        assert!(fresh.warm(graph.state()).is_empty());
    }

    #[test]
    fn kept_warm_and_bound_scenes_rank_after_preview_and_before_the_recent_ones() {
        use jstream_protocol::{Action, Chord, SceneOptions};
        let mut graph = Graph::new();
        apply(
            &mut graph,
            ["a", "b", "c", "d", "e"].into_iter().map(create).collect(),
        );
        let mut studio = Studio::default();
        studio.reconcile(graph.state());
        studio.switch(scene("b"), None);
        studio.switch(scene("a"), None);
        studio
            .set_preview(graph.state(), &scene("c"))
            .expect("c exists");
        assert_eq!(studio.warm(graph.state()), vec![scene("c"), scene("b")]);

        apply(
            &mut graph,
            vec![
                Edit::SceneSetOptions {
                    scene_id: scene("e"),
                    options: SceneOptions {
                        keep_warm: true,
                        ..SceneOptions::default()
                    },
                },
                Edit::HotkeySet {
                    chord: Chord::new("F1"),
                    action: Action::SceneSwitch {
                        scene_id: scene("d"),
                        preset_id: None,
                    },
                },
            ],
        );
        assert_eq!(
            studio.warm(graph.state()),
            vec![scene("c"), scene("e"), scene("d")],
            "preview, kept warm, bound; the recent b is past the budget of 3"
        );
        studio.set_budget(8);
        assert_eq!(
            studio.warm(graph.state()),
            vec![scene("c"), scene("e"), scene("d"), scene("b")]
        );
        studio.switch(scene("e"), None);
        assert_eq!(
            studio.warm(graph.state()),
            vec![scene("c"), scene("d"), scene("a"), scene("b")],
            "a kept-warm scene on program is the program's"
        );
    }
}
