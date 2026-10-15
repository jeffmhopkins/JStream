//! Studio state: which scene is on program (ADR-0030).
//!
//! The program scene is runtime state, beside the outputs' state, not collection state:
//! `scene.switch` changes it without a new `rev`, and undo never touches it. It always
//! names a scene the collection has, whenever the collection has one. The collection's
//! first scene goes on program when it is created, and when a batch deletes the program
//! scene, program moves to the first scene left in display order. Reordering scenes
//! never moves it. Only a cut exists so far: a switch shows the new scene from the next
//! frame.

use jstream_graph::State;
use jstream_protocol::{EntityKind, ProtocolError, SceneId};

/// The studio state the engine keeps under its collection lock.
#[derive(Debug, Default)]
pub(crate) struct Studio {
    program: Option<SceneId>,
}

impl Studio {
    /// The scene on program, or `None` while the collection has no scene.
    pub(crate) fn program(&self) -> Option<&SceneId> {
        self.program.as_ref()
    }

    /// Puts `scene_id` on program. `Ok(true)` when program changed, `Ok(false)` when the
    /// scene was already on program, and `not_found` when the collection has no such
    /// scene.
    pub(crate) fn switch(
        &mut self,
        collection: &State,
        scene_id: &SceneId,
    ) -> Result<bool, ProtocolError> {
        if collection.scene(scene_id).is_none() {
            return Err(ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: scene_id.as_str().to_owned(),
            });
        }
        if self.program.as_ref() == Some(scene_id) {
            return Ok(false);
        }
        self.program = Some(scene_id.clone());
        Ok(true)
    }

    /// Follows the collection after it changed. A program scene the collection no
    /// longer has gives way to its first scene, and an empty program takes the first
    /// scene once there is one. `true` when program changed.
    pub(crate) fn reconcile(&mut self, collection: &State) -> bool {
        if let Some(id) = &self.program
            && collection.scene(id).is_some()
        {
            return false;
        }
        let next = collection.scenes().first().map(|scene| scene.id().clone());
        if next == self.program {
            return false;
        }
        self.program = next;
        true
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
    fn switching_names_an_existing_scene_and_is_idempotent() {
        let mut graph = Graph::new();
        let mut studio = Studio::default();
        apply(&mut graph, vec![create("a"), create("b")]);
        studio.reconcile(graph.state());

        assert_eq!(studio.switch(graph.state(), &scene("b")), Ok(true));
        assert_eq!(studio.switch(graph.state(), &scene("b")), Ok(false));
        assert_eq!(studio.program(), Some(&scene("b")));
        assert_eq!(
            studio.switch(graph.state(), &scene("ghost")),
            Err(ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: "ghost".into(),
            })
        );
        assert_eq!(studio.program(), Some(&scene("b")));
    }

    #[test]
    fn deleting_the_program_scene_moves_program_to_the_first_scene() {
        let mut graph = Graph::new();
        let mut studio = Studio::default();
        apply(&mut graph, vec![create("a"), create("b"), create("c")]);
        studio.reconcile(graph.state());
        studio.switch(graph.state(), &scene("c")).expect("c exists");

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
