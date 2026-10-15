//! Reading the graph as protocol query results: what an engine answers to the queries
//! that read collection state (`scene.list`, `source.list`, `item.list`, `output.list`,
//! `encoder.list`, `bus.list`, `state.get`), to the kind queries (`kind.list`,
//! `kind.describe`), and to `delete.plan` (`plan.rs`).
//!
//! Reads never change the graph. Each collection result carries the revision it was
//! read at; kinds are not collection state, so their results carry none.

use crate::model::{Bus, Encoder, Filter, Graph, Item, Output, Placement, Scene, Source, State};
use jstream_protocol::{
    BusInfo, EncoderProfileInfo, EntityKind, FilterChains, FilterInfo, ItemInfo, KindDescriptor,
    OutputInfo, ProtocolError, Query, QueryResult, SceneId, SceneInfo, SceneSummary, SendInfo,
    SourceInfo,
};

impl Graph {
    /// Answers `query` if it reads collection state, or `None` if it reads something the
    /// graph doesn't hold (`output.status` reads runtime state) or is a query this crate
    /// doesn't know.
    ///
    /// Fails only with `not_found`, for an `item.list` of a missing scene or a
    /// `delete.plan` of a missing target, and with `invalid_argument` on `kind_id`, for a
    /// `kind.describe` of a kind it doesn't have.
    pub fn query(&self, query: &Query) -> Option<Result<QueryResult, ProtocolError>> {
        Some(match query {
            Query::SceneList => Ok(self.scene_list()),
            Query::SourceList => Ok(self.source_list()),
            Query::ItemList { scene_id } => self.item_list(scene_id),
            Query::StateGet => Ok(self.state_get()),
            Query::KindList => Ok(self.kind_list()),
            Query::KindDescribe { kind_id } => self.kind_describe(kind_id),
            Query::OutputList => Ok(self.output_list()),
            Query::EncoderList => Ok(self.encoder_list()),
            Query::BusList => Ok(self.bus_list()),
            Query::DeletePlan { target } => self.delete_plan(target),
            _ => return None,
        })
    }

    /// The `kind.list` result: every kind, ordered by ID.
    pub fn kind_list(&self) -> QueryResult {
        QueryResult::KindList {
            kinds: self.kinds().map(KindDescriptor::summary).collect(),
        }
    }

    /// The `kind.describe` result for `kind_id`: its descriptor and settings schema.
    /// Fails with `invalid_argument` on `kind_id` if there is no such kind.
    pub fn kind_describe(&self, kind_id: &str) -> Result<QueryResult, ProtocolError> {
        let kind = self.kind(kind_id).ok_or_else(|| {
            ProtocolError::invalid_argument(
                "kind_id",
                format!("there is no kind `{kind_id}`; `kind.list` names them"),
            )
        })?;
        Ok(QueryResult::KindDescribe {
            settings_schema: kind.settings_schema(),
            kind: kind.clone(),
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
                    role: scene.role,
                    item_count: u32::try_from(scene.items.len()).unwrap_or(u32::MAX),
                })
                .collect(),
        }
    }

    /// The `source.list` result: every source, ordered by ID.
    pub fn source_list(&self) -> QueryResult {
        QueryResult::SourceList {
            rev: self.rev,
            sources: self
                .state
                .sources()
                .map(|s| s.to_info(&self.state))
                .collect(),
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

    /// The `output.list` result: every output, ordered by ID.
    pub fn output_list(&self) -> QueryResult {
        QueryResult::OutputList {
            rev: self.rev,
            outputs: self.state.outputs().map(Output::to_info).collect(),
        }
    }

    /// The `encoder.list` result: every encoder profile, ordered by ID.
    pub fn encoder_list(&self) -> QueryResult {
        QueryResult::EncoderList {
            rev: self.rev,
            encoders: self.state.encoders().map(Encoder::to_info).collect(),
        }
    }

    /// The `bus.list` result: every bus, ordered by ID, and every send, ordered by bus,
    /// then by source.
    pub fn bus_list(&self) -> QueryResult {
        QueryResult::BusList {
            rev: self.rev,
            buses: self.state.buses().map(Bus::to_info).collect(),
            sends: self.send_infos(),
        }
    }

    fn send_infos(&self) -> Vec<SendInfo> {
        self.state
            .sends()
            .map(|(source, bus, gain_db)| SendInfo {
                source_id: source.clone(),
                bus_id: bus.clone(),
                gain_db,
            })
            .collect()
    }

    /// The `state.get` result: the canvas, every source by ID, every scene with its items
    /// in display order, every output, encoder profile and bus by ID, and every send.
    pub fn state_get(&self) -> QueryResult {
        QueryResult::StateGet {
            rev: self.rev,
            canvas: self.state.canvas(),
            sources: self
                .state
                .sources()
                .map(|s| s.to_info(&self.state))
                .collect(),
            scenes: self.state.scenes().iter().map(Scene::to_info).collect(),
            outputs: self.state.outputs().map(Output::to_info).collect(),
            encoders: self.state.encoders().map(Encoder::to_info).collect(),
            buses: self.state.buses().map(Bus::to_info).collect(),
            sends: self.send_infos(),
        }
    }
}

impl Source {
    /// This source as query results report it, in `state`, which holds it: its
    /// `duplicated_from` only while `state` still has that source.
    pub fn to_info(&self, state: &State) -> SourceInfo {
        SourceInfo {
            source_id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            settings: self.settings.clone(),
            duplicated_from: self
                .duplicated_from
                .clone()
                .filter(|original| state.source(original).is_some()),
            audio: self.audio,
            filters: FilterChains {
                video: self.filters.video.iter().map(Filter::to_info).collect(),
                audio: self.filters.audio.iter().map(Filter::to_info).collect(),
            },
        }
    }
}

impl Filter {
    /// This filter as query results report it.
    pub fn to_info(&self) -> FilterInfo {
        FilterInfo {
            filter_id: self.id.clone(),
            kind: self.kind.clone(),
            enabled: self.enabled,
            settings: self.settings.clone(),
        }
    }
}

impl Output {
    /// This output as `output.list` and `state.get` report it.
    pub fn to_info(&self) -> OutputInfo {
        OutputInfo {
            output_id: self.id.clone(),
            kind: self.kind.clone(),
            encoder_profile: self.encoder_profile.clone(),
        }
    }
}

impl Encoder {
    /// This profile as `encoder.list` and `state.get` report it.
    pub fn to_info(&self) -> EncoderProfileInfo {
        EncoderProfileInfo {
            profile_id: self.id.clone(),
            name: self.name.clone(),
            profile: self.profile,
        }
    }
}

impl Bus {
    /// This bus as `bus.list` and `state.get` report it.
    pub fn to_info(&self) -> BusInfo {
        BusInfo {
            bus_id: self.id.clone(),
            name: self.name.clone(),
            gain_db: self.gain_db,
            muted: self.muted,
        }
    }
}

impl Scene {
    /// This scene and its items as `state.get` reports them.
    pub fn to_info(&self) -> SceneInfo {
        SceneInfo {
            scene_id: self.id.clone(),
            name: self.name.clone(),
            role: self.role,
            items: self.items.iter().map(Item::to_info).collect(),
        }
    }
}

impl Item {
    /// This item as query results report it.
    ///
    /// A geometry item's deprecated `transform` is the view computed from its geometry
    /// alone (ADR-0022), so it changes only with an edit.
    pub fn to_info(&self) -> ItemInfo {
        let (transform, geometry) = match self.placement {
            Placement::Legacy(transform) => (transform, None),
            Placement::Geometry(geometry) => (
                // `item.add` and `item.set_geometry` reject a geometry whose corners are
                // not finite, so the fallback is never taken.
                geometry
                    .transform_view()
                    .unwrap_or_else(|_| centre_view(geometry)),
                Some(geometry),
            ),
        };
        ItemInfo {
            item_id: self.id.clone(),
            source_id: self.source_id.clone(),
            placement: self.placement.form(),
            transform,
            geometry,
            crop: self.crop,
            visible: self.visible,
            opacity: self.opacity,
            blend: self.blend,
        }
    }
}

/// A view at the box's centre, for a geometry whose corner is not finite.
fn centre_view(geometry: jstream_protocol::ItemGeometry) -> jstream_protocol::ItemTransform {
    jstream_protocol::ItemTransform {
        position: geometry.center,
        rotation_deg: geometry.rotation_deg,
        ..jstream_protocol::ItemTransform::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use crate::Graph;
    use jstream_protocol::{
        Edit, ItemId, OutputId, ProtocolError, Query, QueryResult, Rev, parity,
    };

    /// The shared parity collection (`jstream_protocol::parity::collection`), which
    /// `jstream-control`'s mock engine is tested against too.
    fn graph() -> Graph {
        let setup = parity::collection();
        let mut graph = Graph::new().with_kinds(parity::kinds());
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
    fn an_empty_graph_reads_empty_but_for_the_master_bus() {
        let graph = Graph::new();
        assert_eq!(
            graph.state_get(),
            QueryResult::StateGet {
                rev: Rev(0),
                canvas: jstream_protocol::Canvas::DEFAULT,
                sources: vec![],
                scenes: vec![],
                outputs: vec![],
                encoders: vec![],
                buses: vec![jstream_protocol::BusInfo {
                    bus_id: jstream_protocol::BusId::master(),
                    name: "Master".into(),
                    gain_db: jstream_protocol::Finite32::ZERO,
                    muted: false,
                }],
                sends: vec![],
            }
        );
        assert_eq!(
            graph.output_list(),
            QueryResult::OutputList {
                rev: Rev(0),
                outputs: vec![],
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
    fn kinds_are_listed_by_id_and_described_with_a_schema() {
        let graph = graph();
        let Some(Ok(QueryResult::KindList { kinds })) = graph.query(&Query::KindList) else {
            panic!("kind.list");
        };
        let mut expected: Vec<_> = parity::kinds().iter().map(|k| k.summary()).collect();
        expected.sort_by(|a, b| a.kind_id.cmp(&b.kind_id));
        assert_eq!(kinds, expected);
        let describe = |id: &str| graph.query(&Query::KindDescribe { kind_id: id.into() });
        let Some(Ok(QueryResult::KindDescribe {
            kind,
            settings_schema,
        })) = describe("webcam")
        else {
            panic!("kind.describe");
        };
        assert_eq!(settings_schema, kind.settings_schema());
        assert_eq!(kind, parity::kinds()[1]);
        assert!(matches!(
            describe("ghost"),
            Some(Err(ProtocolError::InvalidArgument { field, .. })) if field == "kind_id"
        ));
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
                ..
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
