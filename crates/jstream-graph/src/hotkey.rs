//! Hotkey bindings (ADR-0024 §5) and controller bindings (ADR-0122): the `hotkey.*` and
//! `binding.*` edits, the labels a hotkey binding carries (ADR-0133), and the removal of
//! the bindings that name a deleted object (ADR-0040 §3).
//!
//! A binding names an object when its action does ([`Action::named_objects`]). Every
//! object a binding names exists: `hotkey.set` checks it, and the delete edits remove
//! the bindings that name their target in the same edit, so no binding is ever left
//! pointing at nothing. Bindings are not delete blockers.

use crate::apply::{already_exists, invalid, not_found};
use crate::model::State;
use jstream_protocol::{
    Action, Binding, Chord, Edit, EntityKind, HotkeyBinding, LabelId, ProtocolError, Trigger,
};

/// Applies `hotkey.set`, `hotkey.set_labels`, `hotkey.clear`, `binding.set` and
/// `binding.clear`, and returns the edits that undo it. `None` for any other edit.
pub(crate) fn apply(
    state: &mut State,
    edit: &Edit,
    at: usize,
) -> Option<Result<Vec<Edit>, ProtocolError>> {
    Some(match edit {
        Edit::HotkeySet { chord, action } => set(state, chord, action, at),
        Edit::HotkeySetLabels { chord, labels } => set_labels(state, chord, labels, at),
        Edit::HotkeyClear { chord } => clear(state, chord),
        Edit::BindingSet { trigger, action } => bind(state, trigger, action, at),
        Edit::BindingClear { trigger } => unbind(state, trigger),
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
    check_bindable(state, action, at)?;
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

fn bind(
    state: &mut State,
    trigger: &Trigger,
    action: &Action,
    at: usize,
) -> Result<Vec<Edit>, ProtocolError> {
    if let Some((field, reason)) = trigger.problem() {
        return Err(invalid(at, &format!("trigger.{field}"), reason));
    }
    if state.bindings.contains_key(trigger) {
        return Err(already_exists(EntityKind::Binding, &trigger.to_string()));
    }
    check_bindable(state, action, at)?;
    state.bindings.insert(trigger.clone(), action.clone());
    Ok(vec![Edit::BindingClear {
        trigger: trigger.clone(),
    }])
}

fn unbind(state: &mut State, trigger: &Trigger) -> Result<Vec<Edit>, ProtocolError> {
    let action = state
        .bindings
        .remove(trigger)
        .ok_or_else(|| not_found(EntityKind::Binding, &trigger.to_string()))?;
    Ok(vec![Edit::BindingSet {
        trigger: trigger.clone(),
        action,
    }])
}

/// Checks that `action` can be bound, for a hotkey or a controller binding: the action's
/// own rule, then that every object it names exists.
fn check_bindable(state: &State, action: &Action, at: usize) -> Result<(), ProtocolError> {
    if let Some((field, reason)) = action.bind_problem() {
        return Err(invalid(at, &format!("action.{field}"), reason));
    }
    for (entity, id) in action.named_objects() {
        if !exists(state, entity, id) {
            return Err(not_found(entity, id));
        }
    }
    Ok(())
}

/// The hotkey and controller bindings a batch's deletes removed, in the order it removed
/// them.
#[derive(Debug, Default)]
pub(crate) struct Removed {
    pub(crate) hotkeys: Vec<HotkeyBinding>,
    pub(crate) bindings: Vec<Binding>,
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
/// Returns the edits that restore them (the hotkeys by chord, each with its labels, then
/// the controller bindings by trigger), to run after the items and timelines are back, and records each
/// removed binding in `removed`.
pub(crate) fn remove_missing_items(state: &mut State, removed: &mut Removed) -> Vec<Edit> {
    let gone = |action: &Action| {
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
    };
    let chords: Vec<Chord> = state
        .hotkeys
        .iter()
        .filter(|(_, action)| gone(action))
        .map(|(chord, _)| chord.clone())
        .collect();
    let triggers: Vec<Trigger> = state
        .bindings
        .iter()
        .filter(|(_, action)| gone(action))
        .map(|(trigger, _)| trigger.clone())
        .collect();
    remove(state, chords, triggers, removed)
}

/// Removes every binding whose action names the object `entity` `id`, which its delete
/// edit has just removed. Returns the edits that restore them (`hotkey.set` ordered by
/// chord, each followed by its `hotkey.set_labels` when it carried labels, then
/// `binding.set` ordered by trigger), to run after the object is re-created, and records
/// each removed binding in `removed`.
pub(crate) fn remove_naming(
    state: &mut State,
    entity: EntityKind,
    id: &str,
    removed: &mut Removed,
) -> Vec<Edit> {
    let names = |action: &Action| action.named_objects().contains(&(entity, id));
    let chords: Vec<Chord> = state
        .hotkeys
        .iter()
        .filter(|(_, action)| names(action))
        .map(|(chord, _)| chord.clone())
        .collect();
    let triggers: Vec<Trigger> = state
        .bindings
        .iter()
        .filter(|(_, action)| names(action))
        .map(|(trigger, _)| trigger.clone())
        .collect();
    remove(state, chords, triggers, removed)
}

/// Removes the bindings of `chords` and `triggers`, records them in `removed`, and
/// returns the edits that restore them.
fn remove(
    state: &mut State,
    chords: Vec<Chord>,
    triggers: Vec<Trigger>,
    removed: &mut Removed,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    for binding in chords.iter().filter_map(|chord| take(state, chord)) {
        removed.hotkeys.push(binding.clone());
        edits.extend(restore(binding));
    }
    for trigger in triggers {
        if let Some(action) = state.bindings.remove(&trigger) {
            removed
                .bindings
                .push(Binding::new(trigger.clone(), &action));
            edits.push(Edit::BindingSet { trigger, action });
        }
    }
    edits
}
