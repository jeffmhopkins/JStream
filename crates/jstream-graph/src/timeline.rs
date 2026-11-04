//! Timelines ([ADR-0115](../../../docs/adr/0115-timelines-on-the-wire.md)): the
//! `timeline.set` and `timeline.delete` edits, and the timelines a scene holds.
//!
//! A timeline belongs to one scene, but its ID is unique across the collection, so an
//! action names it alone. The objects its cues name are not checked: a cue naming a
//! deleted item stays, inert, as a follow camera's item does (ADR-0113), so deleting an
//! object never edits a timeline, and an undo or a load can restore timelines in any order.
//! The timing rules are `jstream-anim`'s, which the engine plays them by, and the
//! protocol's [`TimelineInfo::problem`] holds them for clients too.

use crate::apply::{already_exists, invalid, not_found};
use crate::model::{Scene, State};
use jstream_protocol::{
    Edit, EntityKind, MAX_TIMELINES, ProtocolError, SceneId, TimelineId, TimelineInfo,
};

/// Applies `timeline.set` and `timeline.delete`, and returns the edits that undo it.
/// `None` for any other edit.
pub(crate) fn apply(
    state: &mut State,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::TimelineSet { scene_id, timeline } => set(state, scene_id, timeline, at),
        Edit::TimelineDelete { timeline_id } => delete(state, timeline_id),
        _ => return None,
    })
}

fn set(
    state: &mut State,
    scene_id: &SceneId,
    timeline: &TimelineInfo,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    let si = state
        .scene_index(scene_id)
        .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))?;
    if let Some((field, reason)) = timeline.problem() {
        return Err(invalid(at, &format!("timeline.{field}"), reason));
    }
    let id = &timeline.timeline_id;
    if let Some((holder, _)) = state.timeline(id)
        && holder.id() != scene_id
    {
        return Err(already_exists(EntityKind::Timeline, id.as_str()));
    }
    let timelines = &mut state.scenes[si].timelines;
    if !timelines.contains_key(id) && timelines.len() >= MAX_TIMELINES {
        return Err(invalid(
            at,
            "timeline",
            "a scene holds at most 32 timelines",
        ));
    }
    let old = timelines.insert(id.clone(), timeline.clone());
    Ok(vec![match old {
        Some(old) => Edit::TimelineSet {
            scene_id: scene_id.clone(),
            timeline: old,
        },
        None => Edit::TimelineDelete {
            timeline_id: id.clone(),
        },
    }])
}

fn delete(state: &mut State, timeline_id: &TimelineId) -> Result<Vec<Edit>, ProtocolError> {
    let scene_id = state
        .timeline(timeline_id)
        .map(|(scene, _)| scene.id().clone())
        .ok_or_else(|| not_found(EntityKind::Timeline, timeline_id.as_str()))?;
    let si = state
        .scene_index(&scene_id)
        .ok_or_else(|| not_found(EntityKind::Scene, scene_id.as_str()))?;
    let Some(old) = state.scenes[si].timelines.remove(timeline_id) else {
        return Err(not_found(EntityKind::Timeline, timeline_id.as_str()));
    };
    Ok(vec![Edit::TimelineSet {
        scene_id,
        timeline: old,
    }])
}

/// The edits that put back a deleted scene's timelines, ordered by ID.
pub(crate) fn restore(scene: &Scene) -> Vec<Edit> {
    scene
        .timelines
        .values()
        .map(|timeline| Edit::TimelineSet {
            scene_id: scene.id.clone(),
            timeline: timeline.clone(),
        })
        .collect()
}

impl State {
    /// The timeline with this ID, and the scene that holds it.
    pub fn timeline(&self, id: &TimelineId) -> Option<(&Scene, &TimelineInfo)> {
        self.scenes
            .iter()
            .find_map(|scene| Some((scene, scene.timelines.get(id)?)))
    }
}
