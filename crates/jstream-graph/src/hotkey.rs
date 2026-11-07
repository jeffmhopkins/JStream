//! Hotkey bindings (ADR-0024 §5): the `hotkey.*` edits, the labels a binding carries
//! (ADR-0133), and the removal of the bindings that name a deleted object (ADR-0040 §3).
//!
//! A binding names an object when its action does ([`Action::named_objects`]). Every
//! object a binding names exists: `hotkey.set` checks it, and the delete edits remove
//! the bindings that name their target in the same edit, so no binding is ever left
//! pointing at nothing. Bindings are not delete blockers.

use crate::apply::{already_exists, invalid, not_found};
use crate::model::State;
use jstream_protocol::{Action, Chord, Edit, EntityKind, HotkeyBinding, LabelId, ProtocolError};

/// Applies `hotkey.set`, `hotkey.set_labels` and `hotkey.clear`, and returns the edits that undo it. `None`
/// for any other edit.
pub(crate) fn apply(
    state: &mut State,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::HotkeySet { chord, action } => set(state, chord, action, at),
        Edit::HotkeySetLabels { chord, labels } => set_labels(state, chord, labels, at),
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

/// Sets the labels of the binding of `chord`, whole: `not_found` for a chord with no
/// binding or a label that doesn't exist, `invalid_argument` for a label listed twice.
/// An empty list removes them. The inverse sets the old ones back.
fn set_labels(
    state: &mut State,
    chord: &Chord,
    labels: &[LabelId],
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    if !state.hotkeys.contains_key(chord) {
        return Err(not_found(EntityKind::Hotkey, chord.as_str()));
    }
    let labels = crate::organise::checked_labels(state, labels, at)?;
    let old = state.hotkey_labels(chord);
    if labels.is_empty() {
        state.hotkey_labels.remove(chord);
    } else {
        state.hotkey_labels.insert(chord.clone(), labels);
    }
    Ok(vec![Edit::HotkeySetLabels {
        chord: chord.clone(),
        labels: old,
    }])
}

fn clear(state: &mut State, chord: &Chord) -> Result<Vec<Edit>, ProtocolError> {
    let removed =
        take(state, chord).ok_or_else(|| not_found(EntityKind::Hotkey, chord.as_str()))?;
    Ok(restore(removed))
}

/// Removes the binding of `chord` and its labels, and returns it.
fn take(state: &mut State, chord: &Chord) -> Option<HotkeyBinding> {
    let action = state.hotkeys.remove(chord)?;
    let labels = state
        .hotkey_labels
        .remove(chord)
        .map(|labels| labels.into_iter().collect())
        .unwrap_or_default();
    Some(HotkeyBinding::new(chord.clone(), &action).with_labels(labels))
}

/// The edits that put back a removed binding: its `hotkey.set`, then its
/// `hotkey.set_labels` when it carried any.
fn restore(binding: HotkeyBinding) -> Vec<Edit> {
    let Some(action) = binding.action() else {
        return Vec::new();
    };
    let mut edits = vec![Edit::HotkeySet {
        chord: binding.chord.clone(),
        action,
    }];
    if !binding.labels.is_empty() {
        edits.push(Edit::HotkeySetLabels {
            chord: binding.chord,
            labels: binding.labels,
        });
    }
    edits
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
        EntityKind::Item => state.item(&jstream_protocol::ItemId::new(id)).is_some(),
        EntityKind::Timeline => state
            .timeline(&jstream_protocol::TimelineId::new(id))
            .is_some(),
        _ => false,
    }
}

/// Removes every binding whose action names an item or a timeline the collection no
/// longer has (ADR-0101, ADR-0115): `item.remove`, and the deletes that take items with
/// them (`scene.delete`, `source.delete`), drop the bindings of `item.show`, `item.hide`
/// and `item.toggle` naming those items, and `timeline.delete` and `scene.delete` those
/// of `timeline.play`, `timeline.stop` and `timeline.seek` naming its timelines.
/// Returns the edits that restore them, ordered by chord, to run after the items and
/// timelines are back, and records each removed binding in `removed`.
pub(crate) fn remove_missing_items(
    state: &mut State,
    removed: &mut Vec<HotkeyBinding>,
) -> Vec<Edit> {
    let chords: Vec<Chord> = state
        .hotkeys
        .iter()
        .filter(|(_, action)| {
            action
                .named_objects()
                .iter()
                .any(|&(entity, id)| match entity {
                    EntityKind::Item => state.item(&jstream_protocol::ItemId::new(id)).is_none(),
                    EntityKind::Timeline => state
                        .timeline(&jstream_protocol::TimelineId::new(id))
                        .is_none(),
                    _ => false,
                })
        })
        .map(|(chord, _)| chord.clone())
        .collect();
    chords
        .into_iter()
        .filter_map(|chord| take(state, &chord))
        .flat_map(|binding| {
            removed.push(binding.clone());
            restore(binding)
        })
        .collect()
}

/// Removes every binding whose action names the object `entity` `id`, which its delete
/// edit has just removed. Returns the edits that restore them (`hotkey.set`, then
/// `hotkey.set_labels` for one that carried labels, ordered by chord), to run after the object is re-created, and records each removed binding in
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
        .filter_map(|chord| take(state, &chord))
        .flat_map(|binding| {
            removed.push(binding.clone());
            restore(binding)
        })
        .collect()
}
