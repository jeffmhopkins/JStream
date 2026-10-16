//! Dependency edges between scenes and sources, for the cycle check and for delete
//! rejection (ADR-0025, docs/adr/0025-typed-settings-kind-descriptors.md).
//!
//! The edges are: a scene depends on each source its items place; a `scene` source
//! depends on the scene it shows; and a source depends on every scene, source and bus
//! its settings refer to. [`edges`] lists what a node depends on, for the cycle check,
//! and [`blockers`] lists what depends on a node, for the delete check and
//! `delete.plan`; both read the same three kinds of edge. A bus depends on nothing, so
//! an edge into one never closes a cycle; it only blocks the bus's delete. Asset
//! references name files, which are outside the state, so they are not edges.

use crate::model::State;
use jstream_protocol::{Blocker, BusId, EntityKind, Reference, SceneId, SourceId, SourceKind};
use std::collections::BTreeSet;
use std::fmt;

/// A scene, a source or a bus: the kinds of object an edge joins. A bus is only ever
/// the end of an edge.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Node {
    Scene(SceneId),
    Source(SourceId),
    Bus(BusId),
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Node::Scene(id) => write!(f, "scene `{id}`"),
            Node::Source(id) => write!(f, "source `{id}`"),
            Node::Bus(id) => write!(f, "bus `{id}`"),
        }
    }
}

/// What `node` depends on directly.
fn edges(state: &State, node: &Node) -> Vec<Node> {
    match node {
        Node::Scene(id) => state
            .scene(id)
            .map(|s| {
                s.items
                    .iter()
                    .map(|i| Node::Source(i.source_id.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        Node::Source(id) => {
            let Some(source) = state.source(id) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            if let SourceKind::Scene { scene_id } = &source.kind {
                out.push(Node::Scene(scene_id.clone()));
            }
            out.extend(
                source
                    .settings
                    .references()
                    .filter_map(|(_, reference)| reference_node(&reference)),
            );
            out
        }
        Node::Bus(_) => Vec::new(),
    }
}

/// The node a settings reference is an edge to, if it is one.
pub(crate) fn reference_node(reference: &Reference) -> Option<Node> {
    match reference {
        Reference::Scene { scene_id } => Some(Node::Scene(scene_id.clone())),
        Reference::Source { source_id } => Some(Node::Source(source_id.clone())),
        Reference::Bus { bus_id } => Some(Node::Bus(bus_id.clone())),
        _ => None,
    }
}

/// If adding the edge `from -> to` would close a cycle, the cycle as text
/// (`scene `a` -> source `sa` -> scene `a``), else `None`. `state` may already hold the
/// edge.
pub(crate) fn cycle(state: &State, from: &Node, to: &Node) -> Option<String> {
    let mut visited = BTreeSet::new();
    let mut path = Vec::new();
    if !dfs(state, to, from, &mut visited, &mut path) {
        return None;
    }
    let mut text = from.to_string();
    for node in &path {
        text.push_str(" -> ");
        text.push_str(&node.to_string());
    }
    Some(text)
}

/// A cycle anywhere in `state`, as [`cycle`] names it, or `None`. Every edit that adds an
/// edge checks it before it applies, so `None` is an invariant of every graph; the
/// property test asserts it after each batch.
#[cfg(test)]
pub(crate) fn any_cycle(state: &State) -> Option<String> {
    let scenes = state.scenes().iter().map(|s| Node::Scene(s.id.clone()));
    let sources = state.sources().map(|s| Node::Source(s.id.clone()));
    scenes.chain(sources).find_map(|node| {
        edges(state, &node)
            .iter()
            .find_map(|next| cycle(state, &node, next))
    })
}

/// Pushes the chain from `at` to `target` onto `path` if there is one.
fn dfs(
    state: &State,
    at: &Node,
    target: &Node,
    visited: &mut BTreeSet<Node>,
    path: &mut Vec<Node>,
) -> bool {
    path.push(at.clone());
    if at == target {
        return true;
    }
    if visited.insert(at.clone()) {
        for next in edges(state, at) {
            if dfs(state, &next, target, visited, path) {
                return true;
            }
        }
    }
    path.pop();
    false
}

/// Everything that stops `target`'s delete, in a stable order: the items that place a
/// source (scene display order, then stacking order), the `scene` sources that show a
/// scene (ID order), then every settings value that refers to the target (source ID
/// order, then parameter order). A bus is held only by settings values (its sends go
/// with it). `delete.plan` removes exactly these.
pub(crate) fn blockers(state: &State, target: &Node) -> Vec<Blocker> {
    let mut out = Vec::new();
    match target {
        Node::Source(source_id) => {
            out.extend(state.placements(source_id).map(|item| Blocker {
                entity: EntityKind::Item,
                id: item.id.as_str().to_owned(),
                field: "source_id".into(),
            }));
        }
        Node::Scene(scene_id) => {
            out.extend(
                state
                    .sources()
                    .filter(
                        |s| matches!(&s.kind, SourceKind::Scene { scene_id: t } if t == scene_id),
                    )
                    .map(|s| Blocker {
                        entity: EntityKind::Source,
                        id: s.id.as_str().to_owned(),
                        field: "kind.scene_id".into(),
                    }),
            );
        }
        Node::Bus(_) => {}
    }
    for source in state.sources() {
        out.extend(
            source
                .settings
                .references()
                .filter(|(_, r)| reference_node(r).as_ref() == Some(target))
                .map(|(param, _)| Blocker {
                    entity: EntityKind::Source,
                    id: source.id.as_str().to_owned(),
                    field: format!("settings.{param}"),
                }),
        );
    }
    out
}
