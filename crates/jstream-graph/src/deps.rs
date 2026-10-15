//! Dependency edges between scenes and sources, for the cycle check and for delete
//! rejection (ADR-0025, docs/adr/0025-typed-settings-kind-descriptors.md).
//!
//! The edges are: a scene depends on each source its items place; a `scene` source
//! depends on the scene it shows; and a source depends on every scene and source its
//! settings refer to. One function, [`edges`], lists them all, so the cycle check and the
//! delete check can't disagree about what counts. Asset references name files, which are
//! outside the state, so they are not edges.

use crate::model::State;
use jstream_protocol::{Reference, SceneId, SourceId, SourceKind};
use std::collections::BTreeSet;
use std::fmt;

/// A scene or a source: the two kinds of object an edge joins.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Node {
    Scene(SceneId),
    Source(SourceId),
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Node::Scene(id) => write!(f, "scene `{id}`"),
            Node::Source(id) => write!(f, "source `{id}`"),
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
            for (_, reference) in source.settings.references() {
                match reference {
                    Reference::Scene { scene_id } => out.push(Node::Scene(scene_id)),
                    Reference::Source { source_id } => out.push(Node::Source(source_id)),
                    _ => {}
                }
            }
            out
        }
    }
}

/// The node a settings reference is an edge to, if it is one.
pub(crate) fn reference_node(reference: &Reference) -> Option<Node> {
    match reference {
        Reference::Scene { scene_id } => Some(Node::Scene(scene_id.clone())),
        Reference::Source { source_id } => Some(Node::Source(source_id.clone())),
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

/// Every settings value that refers to `target`, as `` `source`.settings.`param` ``, in
/// source ID order.
pub(crate) fn referrers(state: &State, target: &Node) -> Vec<String> {
    state
        .sources()
        .flat_map(|source| {
            source
                .settings
                .references()
                .filter(|(_, r)| reference_node(r).as_ref() == Some(target))
                .map(|(param, _)| format!("{}.settings.{param}", source.id))
                .collect::<Vec<_>>()
        })
        .collect()
}
