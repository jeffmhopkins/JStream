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

use jstream_graph::State;
use jstream_protocol::{EntityKind, ProtocolError, SceneId};

use crate::transition::Running;

/// The studio state the engine keeps under its collection lock.
#[derive(Debug, Default)]
pub(crate) struct Studio {
    program: Option<SceneId>,
    preview: Option<SceneId>,
    running: Option<Running>,
}

impl Studio {
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
        self.program = Some(scene_id);
        self.running = running;
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
    /// dropped: the new one shows at once. `true` when program or preview changed.
    pub(crate) fn reconcile(&mut self, collection: &State) -> bool {
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
}
