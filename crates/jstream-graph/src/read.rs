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
    use jstream_protocol::{
        Crop, Edit, EntityKind, ItemId, ItemInfo, ItemTransform, OutputId, ProtocolError, Query,
        QueryResult, Rev, SceneId, SceneInfo, SceneSummary, SourceId, SourceInfo, SourceKind, Vec2,
    };

    fn cam() -> SourceInfo {
        SourceInfo {
            source_id: SourceId::new("cam"),
            name: "Camera".into(),
            kind: SourceKind::Builtin {
                kind: "webcam".into(),
            },
        }
    }

    fn nested_b() -> SourceInfo {
        SourceInfo {
            source_id: SourceId::new("b_src"),
            name: "Scene B".into(),
            kind: SourceKind::Scene {
                scene_id: SceneId::new("b"),
            },
        }
    }

    fn moved() -> ItemTransform {
        ItemTransform {
            position: Vec2::new(10.0, 20.0).expect("finite"),
            ..ItemTransform::IDENTITY
        }
    }

    const CROP: Crop = Crop {
        left: 1,
        top: 2,
        right: 3,
        bottom: 4,
    };

    /// Scenes `b` then `a` (reordered), source `cam` placed twice in `a` (the second one
    /// moved, cropped, hidden and reordered to the bottom) and scene `b` nested in `a`.
    fn graph() -> Graph {
        let source = |s: &SourceInfo| Edit::SourceCreate {
            source_id: s.source_id.clone(),
            name: s.name.clone(),
            kind: s.kind.clone(),
        };
        let add = |item: &str, source: &str| Edit::ItemAdd {
            scene_id: SceneId::new("a"),
            item_id: ItemId::new(item),
            source_id: SourceId::new(source),
            transform: None,
        };
        let edits = vec![
            Edit::SceneCreate {
                scene_id: SceneId::new("a"),
                name: "A".into(),
            },
            Edit::SceneCreate {
                scene_id: SceneId::new("b"),
                name: "B".into(),
            },
            source(&nested_b()),
            source(&cam()),
            add("i1", "cam"),
            add("i2", "b_src"),
            add("i3", "cam"),
            Edit::ItemSetTransform {
                item_id: ItemId::new("i3"),
                transform: moved(),
            },
            Edit::ItemSetCrop {
                item_id: ItemId::new("i3"),
                crop: CROP,
            },
            Edit::ItemSetVisible {
                item_id: ItemId::new("i3"),
                visible: false,
            },
            Edit::ItemReorder {
                item_id: ItemId::new("i3"),
                index: 0,
            },
            Edit::SceneReorder {
                scene_id: SceneId::new("b"),
                index: 0,
            },
        ];
        let mut graph = Graph::new();
        graph.apply(&edits, None).expect("applies");
        graph
    }

    /// Scene `a`'s items in `graph()`, bottom first.
    fn items() -> Vec<ItemInfo> {
        let plain = |item: &str, source: &str| ItemInfo {
            item_id: ItemId::new(item),
            source_id: SourceId::new(source),
            transform: ItemTransform::IDENTITY,
            crop: Crop::NONE,
            visible: true,
        };
        vec![
            ItemInfo {
                item_id: ItemId::new("i3"),
                source_id: SourceId::new("cam"),
                transform: moved(),
                crop: CROP,
                visible: false,
            },
            plain("i1", "cam"),
            plain("i2", "b_src"),
        ]
    }

    #[test]
    fn reads_report_the_model_at_its_rev() {
        let graph = graph();
        // Sources by ID, whatever order they were created in.
        assert_eq!(
            graph.query(&Query::SourceList),
            Some(Ok(QueryResult::SourceList {
                rev: Rev(1),
                sources: vec![nested_b(), cam()],
            }))
        );
        assert_eq!(
            graph.query(&Query::ItemList {
                scene_id: SceneId::new("a")
            }),
            Some(Ok(QueryResult::ItemList {
                rev: Rev(1),
                scene_id: SceneId::new("a"),
                items: items(),
            }))
        );
        assert_eq!(
            graph.query(&Query::SceneList),
            Some(Ok(QueryResult::SceneList {
                rev: Rev(1),
                scenes: vec![
                    SceneSummary {
                        scene_id: SceneId::new("b"),
                        name: "B".into(),
                        item_count: 0,
                    },
                    SceneSummary {
                        scene_id: SceneId::new("a"),
                        name: "A".into(),
                        item_count: 3,
                    },
                ],
            }))
        );
        assert_eq!(
            graph.query(&Query::StateGet),
            Some(Ok(QueryResult::StateGet {
                rev: Rev(1),
                sources: vec![nested_b(), cam()],
                scenes: vec![
                    SceneInfo {
                        scene_id: SceneId::new("b"),
                        name: "B".into(),
                        items: vec![],
                    },
                    SceneInfo {
                        scene_id: SceneId::new("a"),
                        name: "A".into(),
                        items: items(),
                    },
                ],
            }))
        );
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
    fn missing_scene_is_not_found_and_runtime_reads_are_not_answered() {
        let graph = graph();
        assert_eq!(
            graph.query(&Query::ItemList {
                scene_id: SceneId::new("ghost")
            }),
            Some(Err(ProtocolError::NotFound {
                entity: EntityKind::Scene,
                id: "ghost".into(),
            }))
        );
        let status = Query::OutputStatus {
            output_id: OutputId::new("out"),
        };
        assert_eq!(graph.query(&status), None);
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
