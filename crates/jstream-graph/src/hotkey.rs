//! Hotkey bindings (ADR-0024 §5): the `hotkey.*` edits, and the removal of the bindings
//! that name a deleted object (ADR-0040 §3).
//!
//! A binding names an object when its action does ([`Action::named_objects`]). Every
//! object a binding names exists: `hotkey.set` checks it, and the delete edits remove
//! the bindings that name their target in the same edit, so no binding is ever left
//! pointing at nothing. Bindings are not delete blockers.

use crate::apply::{already_exists, invalid, not_found};
use crate::model::State;
use jstream_protocol::{Action, Chord, Edit, EntityKind, HotkeyBinding, ProtocolError};

/// Applies `hotkey.set` and `hotkey.clear`, and returns the edits that undo it. `None`
/// for any other edit.
pub(crate) fn apply(
    state: &mut State,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::HotkeySet { chord, action } => set(state, chord, action, at),
        Edit::HotkeyClear { chord } => clear(state, chord),
        _ => return None,
    })
}

fn set(
    state: &mut State,
    chord: &Chord,
    action: &Action,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    if let Some(reason) = chord.problem() {
        return Err(invalid(at, "chord", reason));
    }
    if state.hotkeys.contains_key(chord) {
        return Err(already_exists(EntityKind::Hotkey, chord.as_str()));
    }
    if let Some((field, reason)) = action.bind_problem() {
        return Err(invalid(at, &format!("action.{field}"), reason));
    }
    for (entity, id) in action.named_objects() {
        if !exists(state, entity, id) {
            return Err(not_found(entity, id));
        }
    }
    state.hotkeys.insert(chord.clone(), action.clone());
    Ok(vec![Edit::HotkeyClear {
        chord: chord.clone(),
    }])
}

fn clear(state: &mut State, chord: &Chord) -> Result<Vec<Edit>, ProtocolError> {
    let action = state
        .hotkeys
        .remove(chord)
        .ok_or_else(|| not_found(EntityKind::Hotkey, chord.as_str()))?;
    Ok(vec![Edit::HotkeySet {
        chord: chord.clone(),
        action,
    }])
}

/// Whether the object an action names exists. An action naming a kind of object that
/// bindings can't refer to yet counts as missing, so it can't be bound.
fn exists(state: &State, entity: EntityKind, id: &str) -> bool {
    match entity {
        EntityKind::Scene => state.scenes.iter().any(|s| s.id.as_str() == id),
        EntityKind::Source => state.sources.keys().any(|s| s.as_str() == id),
        EntityKind::Output => state.outputs.keys().any(|o| o.as_str() == id),
        EntityKind::Encoder => state.encoders.keys().any(|e| e.as_str() == id),
        EntityKind::Transition => state.transitions.presets.keys().any(|p| p.as_str() == id),
        _ => false,
    }
}

/// Removes every binding whose action names the object `entity` `id`, which its delete
/// edit has just removed. Returns the edits that restore them (`hotkey.set`, ordered by
/// chord), to run after the object is re-created, and records each removed binding in
/// `removed`.
pub(crate) fn remove_naming(
    state: &mut State,
    entity: EntityKind,
    id: &str,
    removed: &mut Vec<HotkeyBinding>,
) -> Vec<Edit> {
    let chords: Vec<Chord> = state
        .hotkeys
        .iter()
        .filter(|(_, action)| action.named_objects().contains(&(entity, id)))
        .map(|(chord, _)| chord.clone())
        .collect();
    chords
        .into_iter()
        .filter_map(|chord| {
            let action = state.hotkeys.remove(&chord)?;
            removed.push(HotkeyBinding::new(chord.clone(), &action));
            Some(Edit::HotkeySet { chord, action })
        })
        .collect()
}
