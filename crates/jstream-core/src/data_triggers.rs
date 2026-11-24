//! Data triggers as the engine runs them
//! ([ADR-0202](../../../docs/adr/0202-live-data-triggers-and-note-learn.md)): a
//! `binding.set` whose trigger is one field of a JTunnel sender's live data meeting a
//! condition runs its action, as a MIDI binding does (`action_bindings.rs`).
//!
//! The `jstream-values` thread ticks it right after the MIDI bindings. Each tick reads
//! each data trigger's field from the engine's [`LiveData`], which holds only the state of
//! senders whose state the person uses (ADR-0171 decision 1), so a sender with "Use this
//! sender's state" off fires nothing.
//!
//! - **`equals`, `rises` and `falls`** fire when their condition starts to hold, and
//!   release (run the action's [`Action::on_release`]) when it stops. With a `*` in the
//!   key, the condition holds while it holds for any matching key.
//! - **`changes`** fires each time a matching key's field takes another value, and has
//!   no release.
//! - What a field holds when the engine starts following a trigger, and what a key
//!   holds when it first arrives, is where it starts from: it fires nothing. A key that
//!   goes (its sender stops, or it goes stale) counts as not holding.
//! - A trigger fires at most once every [`MIN_GAP`], so a field that flickers doesn't
//!   run its action on every message. A press held back by the gap runs once the gap is
//!   over if its condition still holds; a `changes` held back is dropped.
//! - A press whose action has a release is held until its condition stops, or until
//!   its binding is changed or removed.
//!
//! Docs: docs/adr/0202-live-data-triggers-and-note-learn.md

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use jstream_graph::Graph;
use jstream_protocol::{Action, DataOn, Trigger};
use jstream_sources::LiveData;
use serde_json::Value;

/// The least time between two firings of one trigger.
pub(crate) const MIN_GAP: Duration = Duration::from_millis(50);

/// One data trigger as the runner follows it.
#[derive(Debug)]
struct Followed {
    /// Each matching key's field, as last read; a key without the field is left out.
    seen: BTreeMap<String, Value>,
    /// Whether its condition held at the last tick.
    holds: bool,
    /// When it last fired.
    fired: Option<Instant>,
    /// A press [`MIN_GAP`] held back, run once the gap is over if its condition still
    /// holds.
    pending: bool,
    /// The action it ran and the release it holds, while pressed.
    held: Option<(Action, Action)>,
}

/// The data triggers between ticks.
#[derive(Debug, Default)]
pub(crate) struct DataTriggers {
    followed: BTreeMap<Trigger, Followed>,
}

impl DataTriggers {
    /// Reads each of `graph`'s data triggers' fields in `data` and returns the actions
    /// to run, presses and releases, in binding order.
    pub(crate) fn tick(&mut self, graph: &Graph, data: &LiveData, now: Instant) -> Vec<Action> {
        let mut run = Vec::new();
        // A trigger no longer bound, or bound to another action, lets go of its press.
        self.followed.retain(|trigger, followed| {
            let bound = graph
                .state()
                .bindings()
                .find(|(t, _)| *t == trigger)
                .map(|(_, action)| action);
            if let Some((action, release)) = &followed.held
                && bound != Some(action)
            {
                run.push(release.clone());
                followed.held = None;
            }
            bound.is_some()
        });
        for (trigger, action) in graph.state().bindings() {
            let Trigger::Data {
                input,
                key,
                field,
                on,
            } = trigger
            else {
                continue;
            };
            let now_seen = read(data, input.as_deref(), key, field);
            let Some(followed) = self.followed.get_mut(trigger) else {
                let holds = holds(on, now_seen.values());
                self.followed.insert(
                    trigger.clone(),
                    Followed {
                        seen: now_seen,
                        holds,
                        fired: None,
                        pending: false,
                        held: None,
                    },
                );
                continue;
            };
            // A key that arrived this tick starts where it is.
            let mut before = followed.seen.clone();
            for (k, v) in &now_seen {
                before.entry(k.clone()).or_insert_with(|| v.clone());
            }
            let was = holds(on, before.values());
            let is = holds(on, now_seen.values());
            let changed = matches!(on, DataOn::Changes)
                && now_seen
                    .iter()
                    .any(|(k, v)| followed.seen.get(k).is_some_and(|old| old != v));
            followed.seen = now_seen;
            followed.holds = is;
            let press = match on {
                DataOn::Changes => changed,
                _ => (!was || followed.pending) && is,
            };
            if !is {
                followed.pending = false;
            }
            let release = !matches!(on, DataOn::Changes) && was && !is;
            if release && let Some((_, release)) = followed.held.take() {
                run.push(release);
            }
            let open = followed
                .fired
                .is_none_or(|at| now.duration_since(at) >= MIN_GAP);
            // A press within the gap waits for it, unless its condition stops first.
            if press && !open && !matches!(on, DataOn::Changes) {
                followed.pending = true;
            }
            if press && open {
                followed.fired = Some(now);
                followed.pending = false;
                run.push(action.clone());
                if !matches!(on, DataOn::Changes)
                    && let Some(release) = action.on_release()
                {
                    followed.held = Some((action.clone(), release));
                }
            }
        }
        run
    }
}

/// Each key matching `key` from `input` (or any sender) with its `field`, by key; a key
/// without the field is left out.
fn read(data: &LiveData, input: Option<&str>, key: &str, field: &str) -> BTreeMap<String, Value> {
    let pick = |message: &Value| -> Option<Value> {
        if field.is_empty() {
            Some(message.clone())
        } else {
            jstream_sources::data::field(message, field).cloned()
        }
    };
    if !key.split('/').any(|segment| segment == "*") {
        return data
            .get_from(input, key)
            .and_then(|message| pick(&message))
            .map(|value| BTreeMap::from([(key.to_owned(), value)]))
            .unwrap_or_default();
    }
    data.entries(input)
        .into_iter()
        .filter(|(k, _)| matches(key, k))
        .filter_map(|(k, message)| Some((k, pick(&message)?)))
        .collect()
}

/// Whether the key `pattern`, whose `*` segments match any one segment, matches `key`.
pub(crate) fn matches(pattern: &str, key: &str) -> bool {
    let mut p = pattern.split('/');
    let mut k = key.split('/');
    loop {
        match (p.next(), k.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if a == "*" || a == b => {}
            _ => return false,
        }
    }
}

/// Whether `on`'s condition holds for any of `values`. `changes` never holds.
fn holds<'a>(on: &DataOn, mut values: impl Iterator<Item = &'a Value>) -> bool {
    let number = |v: &Value| match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    };
    match on {
        DataOn::Equals { value } => values.any(|v| equals(v, value)),
        DataOn::Changes => false,
        DataOn::Rises { level } => {
            values.any(|v| number(v).is_some_and(|n| n >= f64::from(level.get())))
        }
        DataOn::Falls { level } => {
            values.any(|v| number(v).is_some_and(|n| n < f64::from(level.get())))
        }
    }
}

/// Whether `v` is what `text` spells: the same text, the number it parses as, or
/// `true` or `false`.
pub(crate) fn equals(v: &Value, text: &str) -> bool {
    match v {
        Value::String(s) => s == text,
        Value::Number(n) => text
            .trim()
            .parse::<f64>()
            .ok()
            .zip(n.as_f64())
            .is_some_and(|(a, b)| a == b),
        Value::Bool(b) => text == if *b { "true" } else { "false" },
        _ => false,
    }
}

#[cfg(test)]
mod tests;
