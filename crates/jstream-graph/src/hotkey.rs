//! Hotkey bindings (ADR-0024 §5) and controller bindings (ADR-0122): the `hotkey.*` and
//! `binding.*` edits, a chord or trigger running several actions in the order they were
//! bound (ADR-0208), the labels a hotkey binding carries (ADR-0133), and the removal of
//! the bindings that name a deleted object (ADR-0040 §3). A wheel chord binds only an
//! action that takes steps (ADR-0112 §3).
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
        Edit::HotkeyClear { chord, action } => clear(state, chord, action.as_ref()),
        Edit::BindingSet { trigger, action } => bind(state, trigger, action, at),
        Edit::BindingClear { trigger, action } => unbind(state, trigger, action.as_ref()),
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
    if state.hotkey(chord).contains(action) {
        return Err(already_exists(EntityKind::Hotkey, chord.as_str()));
    }
    check_bindable(state, action, chord.is_wheel(), at)?;
    state
        .hotkeys
        .entry(chord.clone())
        .or_default()
        .push(action.clone());
    Ok(vec![Edit::HotkeyClear {
        chord: chord.clone(),
        action: Some(action.clone()),
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

fn clear(
    state: &mut State,
    chord: &Chord,
    action: Option<&Action>,
) -> Result<Vec<Edit>, ProtocolError> {
    take(state, chord, |bound| {
        action.is_none_or(|action| bound == action)
    })
    .map(|(_, restore)| restore)
    .ok_or_else(|| not_found(EntityKind::Hotkey, chord.as_str()))
}

/// Removes the actions of `chord` that `matches` picks, and the chord's labels when it
/// is left with none. Returns the removed bindings, each with the chord's labels, and
/// the edits that restore the chord exactly: a `hotkey.clear` of what it kept, if it
/// kept any, then a `hotkey.set` for each action it had, in their order, then a
/// `hotkey.set_labels` when it carried labels. `None`, changing nothing, when no action
/// matches.
fn take(
    state: &mut State,
    chord: &Chord,
    matches: impl Fn(&Action) -> bool,
) -> Option<(Vec<HotkeyBinding>, Vec<Edit>)> {
    let labels = state.hotkey_labels(chord);
    let actions = state.hotkeys.get_mut(chord)?;
    if !actions.iter().any(&matches) {
        return None;
    }
    let had = std::mem::take(actions);
    let kept: Vec<Action> = had.iter().filter(|a| !matches(a)).cloned().collect();
    let first = had.iter().position(&matches).unwrap_or_default();
    let emptied = kept.is_empty() || first == 0;
    // Exactly the order it had: the kept actions after the first removed one leave, so
    // the removed ones rejoin in their places. An action bound since stays bound.
    let mut restore: Vec<Edit> = had[first..]
        .iter()
        .filter(|a| !matches(a))
        .map(|action| Edit::HotkeyClear {
            chord: chord.clone(),
            action: Some(action.clone()),
        })
        .collect();
    if kept.is_empty() {
        state.hotkeys.remove(chord);
        state.hotkey_labels.remove(chord);
    } else {
        *actions = kept;
    }
    let removed = had
        .iter()
        .filter(|a| matches(a))
        .map(|action| HotkeyBinding::new(chord.clone(), action).with_labels(labels.clone()))
        .collect();
    restore.extend(had[first..].iter().map(|action| Edit::HotkeySet {
        chord: chord.clone(),
        action: action.clone(),
    }));
    if emptied && !labels.is_empty() {
        restore.push(Edit::HotkeySetLabels {
            chord: chord.clone(),
            labels,
        });
    }
    Some((removed, restore))
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
    if state.trigger_actions(trigger).contains(action) {
        return Err(already_exists(EntityKind::Binding, &trigger.to_string()));
    }
    check_bindable(state, action, false, at)?;
    state
        .bindings
        .entry(trigger.clone())
        .or_default()
        .push(action.clone());
    Ok(vec![Edit::BindingClear {
        trigger: trigger.clone(),
        action: Some(action.clone()),
    }])
}

fn unbind(
    state: &mut State,
    trigger: &Trigger,
    action: Option<&Action>,
) -> Result<Vec<Edit>, ProtocolError> {
    take_trigger(state, trigger, |bound| {
        action.is_none_or(|action| bound == action)
    })
    .map(|(_, restore)| restore)
    .ok_or_else(|| not_found(EntityKind::Binding, &trigger.to_string()))
}

/// Removes the actions of `trigger` that `matches` picks, and the trigger when it is
/// left with none. Returns the removed actions and the edits that restore the trigger
/// exactly: a `binding.clear` of what it kept, if it kept any, then a `binding.set` for
/// each action it had, in their order. `None`, changing nothing, when no action matches.
fn take_trigger(
    state: &mut State,
    trigger: &Trigger,
    matches: impl Fn(&Action) -> bool,
) -> Option<(Vec<Action>, Vec<Edit>)> {
    let actions = state.bindings.get_mut(trigger)?;
    if !actions.iter().any(&matches) {
        return None;
    }
    let had = std::mem::take(actions);
    let kept: Vec<Action> = had.iter().filter(|a| !matches(a)).cloned().collect();
    let first = had.iter().position(&matches).unwrap_or_default();
    let mut restore: Vec<Edit> = had[first..]
        .iter()
        .filter(|a| !matches(a))
        .map(|action| Edit::BindingClear {
            trigger: trigger.clone(),
            action: Some(action.clone()),
        })
        .collect();
    if kept.is_empty() {
        state.bindings.remove(trigger);
    } else {
        *actions = kept;
    }
    let removed = had.iter().filter(|a| matches(a)).cloned().collect();
    restore.extend(had[first..].iter().map(|action| Edit::BindingSet {
        trigger: trigger.clone(),
        action: action.clone(),
    }));
    Some((removed, restore))
}

/// Checks that `action` can be bound, for a hotkey or a controller binding: the action's
/// own rule, then for a `wheel` chord that it takes steps, then that every object it
/// names exists.
fn check_bindable(
    state: &State,
    action: &Action,
    wheel: bool,
    at: usize,
) -> Result<(), ProtocolError> {
    if let Some((field, reason)) = action.bind_problem() {
        return Err(invalid(at, &format!("action.{field}"), reason));
    }
    if wheel && !action.takes_steps() {
        return Err(invalid(
            at,
            "action.type",
            "a wheel chord runs once per notch, so it can only be bound to an action that \
             takes one step, such as `scene.zoom_camera` (ADR-0112)",
        ));
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
        EntityKind::Channel => state.channel(id).is_some(),
        EntityKind::Bus => state.buses.keys().any(|b| b.as_str() == id),
        EntityKind::Show => state.shows.contains_key(&jstream_protocol::ShowId::new(id)),
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
    let gone: Vec<Action> = state
        .hotkeys()
        .map(|(_, action)| action)
        .chain(state.bindings().map(|(_, action)| action))
        .filter(|action| gone(action))
        .cloned()
        .collect();
    remove(state, |action| gone.contains(action), removed)
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
    remove(
        state,
        |action| action.named_objects().contains(&(entity, id)),
        removed,
    )
}

/// Removes every bound action `matches` picks, from every chord and trigger (a chord or
/// trigger keeps its other actions, ADR-0208), records the removed bindings in
/// `removed`, and returns the edits that restore them.
fn remove(
    state: &mut State,
    matches: impl Fn(&Action) -> bool,
    removed: &mut Removed,
) -> Vec<Edit> {
    let chords: Vec<Chord> = state
        .hotkeys()
        .filter(|(_, action)| matches(action))
        .map(|(chord, _)| chord.clone())
        .collect();
    let triggers: Vec<Trigger> = state
        .bindings()
        .filter(|(_, action)| matches(action))
        .map(|(trigger, _)| trigger.clone())
        .collect();
    let mut edits = Vec::new();
    for chord in dedup(chords) {
        if let Some((bindings, restore)) = take(state, &chord, &matches) {
            removed.hotkeys.extend(bindings);
            edits.extend(restore);
        }
    }
    for trigger in dedup(triggers) {
        if let Some((actions, restore)) = take_trigger(state, &trigger, &matches) {
            removed.bindings.extend(
                actions
                    .iter()
                    .map(|action| Binding::new(trigger.clone(), action)),
            );
            edits.extend(restore);
        }
    }
    edits
}

/// `items` with each repeat dropped, the first kept: the input is already in order.
fn dedup<T: PartialEq>(mut items: Vec<T>) -> Vec<T> {
    items.dedup();
    items
}
