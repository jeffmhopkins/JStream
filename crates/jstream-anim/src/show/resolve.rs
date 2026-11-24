//! What a show puts up at a point of show time (ST2, "jump equals play").

use std::collections::BTreeMap;

use super::plan::{Plan, ShowKeys};

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
#[derive(Clone, Debug, PartialEq, Eq, Default)]
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

/// What spans `at_ms` on `edge`'s side.
pub fn spanning<K: ShowKeys>(plan: &Plan<K>, at_ms: u32, edge: Edge) -> Spanning<K::Timeline> {
    todo!()
}

/// The state events before `at_ms` (strictly), last per state key, as event indices in
/// time order. One-shots are left out.
pub fn state_events_before<K: ShowKeys>(plan: &Plan<K>, at_ms: u32) -> Vec<usize> {
    todo!()
}
