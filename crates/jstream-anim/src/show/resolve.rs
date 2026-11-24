//! What a show puts up at a point of show time (ST2, "jump equals play").

use std::collections::BTreeMap;

use super::plan::{Plan, ShowKeys};
use crate::CueAction;

/// Which side of a point to resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Just before `t`: a clip spans `t` when `start < t <= end`, a marker counts when
    /// it is before `t`. What a pausing landing puts up. At `t = 0` nothing spans.
    Before,
    /// At `t`: a clip spans `t` when `start <= t < end`, a marker counts at or before
    /// `t`. What a play-on landing puts up.
    At,
}

/// What spans a point: the show's state there, by clip index into the plan's lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spanning<T: Ord> {
    /// The program clip, and how far into it the point is, in milliseconds.
    pub program: Option<(usize, u32)>,
    /// The keyframe marker in force: the last one inside the program clip's span up to
    /// the point.
    pub keyframe: Option<usize>,
    /// The overlay clips spanning it, and how far into each, by index.
    pub overlays: BTreeMap<usize, u32>,
    /// The sequence clips spanning it, and how far into each, by their sequence; if two
    /// clips of one sequence span it, the later-starting one.
    pub sequences: BTreeMap<T, (usize, u32)>,
}

impl<T: Ord> Default for Spanning<T> {
    fn default() -> Self {
        Spanning {
            program: None,
            keyframe: None,
            overlays: BTreeMap::new(),
            sequences: BTreeMap::new(),
        }
    }
}

/// Whether a span `[start, start + length)` spans `t` on `edge`'s side.
fn spans(start: u32, length: u32, t: u32, edge: Edge) -> bool {
    let end = start.saturating_add(length);
    match edge {
        Edge::Before => start < t && t <= end,
        Edge::At => start <= t && t < end,
    }
}

/// Whether a marker at `at` counts at `t` on `edge`'s side.
fn counts(at: u32, t: u32, edge: Edge) -> bool {
    match edge {
        Edge::Before => at < t,
        Edge::At => at <= t,
    }
}

/// What spans `at_ms` on `edge`'s side.
pub fn spanning<K: ShowKeys>(plan: &Plan<K>, at_ms: u32, edge: Edge) -> Spanning<K::Timeline> {
    let mut out = Spanning::default();
    let mut start = 0u32;
    for (i, clip) in plan.program.iter().enumerate() {
        if spans(start, clip.length_ms, at_ms, edge) {
            out.program = Some((i, at_ms.saturating_sub(start)));
            let end = start.saturating_add(clip.length_ms);
            out.keyframe = plan
                .keyframes
                .iter()
                .rposition(|m| start <= m.at_ms && m.at_ms < end && counts(m.at_ms, at_ms, edge));
            break;
        }
        start = start.saturating_add(clip.length_ms);
    }
    for (i, clip) in plan.overlays.iter().enumerate() {
        if spans(clip.start_ms, clip.length_ms, at_ms, edge) {
            out.overlays.insert(i, at_ms.saturating_sub(clip.start_ms));
        }
    }
    for (i, clip) in plan.sequences.iter().enumerate() {
        if !spans(clip.start_ms, clip.length_ms, at_ms, edge) {
            continue;
        }
        let later = out.sequences.get(&clip.timeline).is_none_or(|&(j, _)| {
            plan.sequences
                .get(j)
                .is_none_or(|c| c.start_ms <= clip.start_ms)
        });
        if later {
            out.sequences.insert(
                clip.timeline.clone(),
                (i, at_ms.saturating_sub(clip.start_ms)),
            );
        }
    }
    out
}

/// The state events before `at_ms` (strictly), last per state key, as event indices in
/// time order. One-shots are left out.
pub fn state_events_before<K: ShowKeys>(plan: &Plan<K>, at_ms: u32) -> Vec<usize> {
    let mut last: Vec<(<K::Action as CueAction>::Key, usize)> = Vec::new();
    for (i, event) in plan.events.iter().enumerate() {
        if event.at_ms >= at_ms {
            continue;
        }
        let Some(key) = event.action.state_key() else {
            continue;
        };
        match last.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = i,
            None => last.push((key, i)),
        }
    }
    let mut out: Vec<usize> = last.into_iter().map(|(_, i)| i).collect();
    out.sort_by_key(|&i| (plan.events.get(i).map_or(0, |e| e.at_ms), i));
    out
}
