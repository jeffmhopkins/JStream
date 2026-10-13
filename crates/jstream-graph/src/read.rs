//! Reading the graph as protocol query results: what an engine answers to the queries
//! that read collection state (`scene.list`, `source.list`, `item.list`, `state.get`).
//!
//! Reads never change the graph. Each result carries the revision it was read at.

use crate::model::{Graph, Item, Scene, Source};
use jstream_protocol::{
    EntityKind, ItemInfo, ProtocolError, Query, QueryResult, SceneId, SceneInfo, SceneSummary,
    SourceInfo,
};

impl Graph {
    /// Answers `query` if it reads collection state, or `None` if it reads something the
    /// graph doesn't hold (`output.status` reads runtime state) or is a query this crate
    /// doesn't know.
    ///
    /// Fails only with `not_found`, for an `item.list` of a missing scene.
    pub fn query(&self, query: &Query) -> Option<Result<QueryResult, ProtocolError>> {
        Some(match query {
            Query::SceneList => Ok(self.scene_list()),
            Query::SourceList => Ok(self.source_list()),
            Query::ItemList { scene_id } => self.item_list(scene_id),
            Query::StateGet => Ok(self.state_get()),
            _ => return None,
        })
    }

    /// The `scene.list` result: every scene in display order, with its item count.
    pub fn scene_list(&self) -> QueryResult {
        QueryResult::SceneList {
            rev: self.rev,
            scenes: self
                .state
                .scenes()
                .iter()
                .map(|scene| SceneSummary {
                    scene_id: scene.id.clone(),
                    name: scene.name.clone(),
                    item_count: u32::try_from(scene.items.len()).unwrap_or(u32::MAX),
                })
                .collect(),
        }
    }

    /// The `source.list` result: every source, ordered by ID.
    pub fn source_list(&self) -> QueryResult {
        QueryResult::SourceList {
            rev: self.rev,
            sources: self.state.sources().map(Source::to_info).collect(),
        }
    }

    /// The `item.list` result for `scene_id`: its items in stacking order, bottom first.
    /// Fails with `not_found` if there is no such scene.
    pub fn item_list(&self, scene_id: &SceneId) -> Result<QueryResult, ProtocolError> {
        let scene = self
            .state
            .scene(scene_id)
            .ok_or_else(|| ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: scene_id.as_str().to_owned(),
            })?;
        Ok(QueryResult::ItemList {
            rev: self.rev,
            scene_id: scene_id.clone(),
            items: scene.items.iter().map(Item::to_info).collect(),
        })
    }

    /// The `state.get` result: every source by ID, and every scene with its items in
    /// display order.
    pub fn state_get(&self) -> QueryResult {
        QueryResult::StateGet {
            rev: self.rev,
            sources: self.state.sources().map(Source::to_info).collect(),
            scenes: self.state.scenes().iter().map(Scene::to_info).collect(),
        }
    }
}

impl Source {
    /// This source as query results report it.
    pub fn to_info(&self) -> SourceInfo {
        SourceInfo {
            source_id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
        }
    }
}

impl Scene {
    /// This scene and its items as `state.get` reports them.
    pub fn to_info(&self) -> SceneInfo {
        SceneInfo {
            scene_id: self.id.clone(),
            name: self.name.clone(),
            items: self.items.iter().map(Item::to_info).collect(),
        }
    }
}

impl Item {
    /// This item as query results report it.
    pub fn to_info(&self) -> ItemInfo {
        ItemInfo {
            item_id: self.id.clone(),
            source_id: self.source_id.clone(),
            transform: self.transform,
            crop: self.crop,
            visible: self.visible,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Graph;
    use jstream_protocol::{Edit, ItemId, OutputId, Query, QueryResult, Rev, parity};

    /// The shared parity collection (`jstream_protocol::parity::collection`), which
    /// `jstream-control`'s mock engine is tested against too.
    fn graph() -> Graph {
        let setup = parity::collection();
        let mut graph = Graph::new();
        let applied = graph
            .apply(&setup.edits, setup.expected_rev)
            .expect("applies");
        assert_eq!(applied.rev, parity::COLLECTION_REV);
        graph
    }

    /// Every read in the shared parity fixture gets exactly the answer the fixture
    /// expects, the same one the mock engine is held to.
    #[test]
    fn reads_report_the_model_at_its_rev() {
        let graph = graph();
        for (query, expected) in parity::reads() {
            assert_eq!(graph.query(&query), Some(expected), "{query:?}");
        }
    }

    /// Every batch the shared parity fixture rejects fails with exactly the expected
    /// error and changes nothing, so every read still holds afterwards.
    #[test]
    fn rejected_batches_match_the_parity_fixture() {
        let mut graph = graph();
        let before = graph.clone();
        for (batch, expected) in parity::rejected() {
            assert_eq!(
                graph.apply(&batch.edits, batch.expected_rev),
                Err(expected),
                "{batch:?}"
            );
            assert_eq!(
                graph, before,
                "a rejected batch changed the graph: {batch:?}"
            );
        }
        for (query, expected) in parity::reads() {
            assert_eq!(graph.query(&query), Some(expected), "{query:?}");
        }
    }

    #[test]
    fn an_empty_graph_reads_empty() {
        let graph = Graph::new();
        assert_eq!(
            graph.state_get(),
            QueryResult::StateGet {
                rev: Rev(0),
                sources: vec![],
                scenes: vec![],
            }
        );
        assert_eq!(
            graph.source_list(),
            QueryResult::SourceList {
                rev: Rev(0),
                sources: vec![],
            }
        );
    }

    #[test]
    fn runtime_reads_are_not_answered() {
        let status = Query::OutputStatus {
            output_id: OutputId::new("out"),
        };
        assert_eq!(graph().query(&status), None);
    }

    #[test]
    fn undo_restores_what_state_get_reads() {
        let mut graph = graph();
        let before = graph.state_get();
        let applied = graph
            .apply(
                &[Edit::ItemRemove {
                    item_id: ItemId::new("i3"),
                }],
                None,
            )
            .expect("removes");
        assert_ne!(graph.state_get(), before);
        graph.apply_batch(&applied.inverse).expect("undoes");
        // Same content; only the revision moved on.
        let (
            QueryResult::StateGet {
                rev,
                sources,
                scenes,
            },
            QueryResult::StateGet {
                sources: was_sources,
                scenes: was_scenes,
                ..
            },
        ) = (graph.state_get(), before)
        else {
            panic!("not state.get results");
        };
        assert_eq!(rev, Rev(3));
        assert_eq!((sources, scenes), (was_sources, was_scenes));
    }
}
