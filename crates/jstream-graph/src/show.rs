//! Shows (ADR-0210): the `show.set` and `show.delete` edits, and the marker naming the
//! internal show scene.
//!
//! A show is a saved run order of scenes; the collection holds only its definition, and
//! running it is the engine's studio state. `show.set` checks its shape. A clip naming a
//! scene, source, sequence, keyframe or preset that doesn't exist is taken, as a
//! timeline's cue is, so deleting a scene never edits a show.

use crate::Graph;
use crate::apply::{invalid, not_found};
use crate::model::State;
use jstream_protocol::{Edit, EntityKind, MAX_SHOWS, ProtocolError, SceneId, ShowId, ShowInfo};

impl Graph {
    /// Every show (ADR-0210), ordered by ID.
    pub fn shows(&self) -> impl ExactSizeIterator<Item = &ShowInfo> {
        self.state.shows.values()
    }

    /// The show with `show_id`, if the collection holds it.
    pub fn show(&self, show_id: &ShowId) -> Option<&ShowInfo> {
        self.state.shows.get(show_id)
    }

    /// Marks `scene_id` as the internal show scene (ADR-0210 decision 12), or clears the
    /// marker. Only the engine sets it; it is saved in the collection file, and is not an
    /// edit or a revision.
    pub fn set_show_scene(&mut self, scene_id: Option<SceneId>) {
        self.state.show_scene = scene_id;
    }
}

impl State {
    /// The internal show scene (ADR-0210 decision 12), if the engine has made one.
    pub fn show_scene(&self) -> Option<&SceneId> {
        self.show_scene.as_ref()
    }
}

/// Applies `show.set` and `show.delete`, and returns the edits that undo it. `None` for
/// any other edit.
pub(crate) fn apply(
    state: &mut State,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::ShowSet { show } => set(state, show, at),
        Edit::ShowDelete { show_id } => delete(state, show_id),
        _ => return None,
    })
}

fn set(state: &mut State, show: &ShowInfo, at: usize) -> Result<Vec<Edit>, ProtocolError> {
    check(state, show, at)?;
    let old = state.shows.insert(show.show_id.clone(), show.clone());
    Ok(vec![match old {
        Some(show) => Edit::ShowSet { show },
        None => Edit::ShowDelete {
            show_id: show.show_id.clone(),
        },
    }])
}

/// Its shape and the collection's room for it.
fn check(state: &State, show: &ShowInfo, at: usize) -> Result<(), ProtocolError> {
    if let Some((field, reason)) = show.problem() {
        return Err(invalid(at, &format!("show.{field}"), reason));
    }
    if !state.shows.contains_key(&show.show_id) && state.shows.len() >= MAX_SHOWS {
        return Err(invalid(
            at,
            "show",
            format!("a collection holds at most {MAX_SHOWS} shows"),
        ));
    }
    Ok(())
}

/// Puts `show` in `state` as a collection file saved it: its shape checked, what it
/// names not looked up.
pub(crate) fn load(state: &mut State, show: &ShowInfo) -> Result<(), ProtocolError> {
    check(state, show, 0)?;
    state.shows.insert(show.show_id.clone(), show.clone());
    Ok(())
}

fn delete(state: &mut State, show_id: &ShowId) -> Result<Vec<Edit>, ProtocolError> {
    let show = state
        .shows
        .remove(show_id)
        .ok_or_else(|| not_found(EntityKind::Show, show_id.as_str()))?;
    Ok(vec![Edit::ShowSet { show }])
}
