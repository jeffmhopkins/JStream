//! The keyframe-scene model ([ADR-0049](../../docs/adr/0049-keyframe-scene-model-and-evaluation.md)):
//! what a variant of a scene (a keyframe, ADR-0024 §2) stores, what its items look like,
//! and what a switch between two keyframes of one scene animates. Layer 0: no
//! dependencies, no `unsafe`, no I/O, so the graph, the engine and a client previewing a
//! keyframe share one definition.
//!
//! - [`Keyframe`] is a variant's override set: per item, an [`Override`] of some of the
//!   five override fields ([`Field`]): the four keyable ones and the effects list
//!   ([ADR-0088](../../docs/adr/0088-keyframe-scenes-override-item-effects.md)), which
//!   replaces the parent's whole and never animates. Writing and clearing a field
//!   return the edit that undoes it ([`Inverse`]); dropping overrides returns what
//!   restores them.
//! - [`Keyframe::effective`] computes an item's values in the variant from the
//!   **parent's current values**: an overridden field takes the override, every other
//!   field follows the parent live (ADR-0049 §1).
//! - [`plan_switch`] classifies each item of a switch between two members of a family
//!   (a parent and its variants) and gives its two ends ([`Step`]). Interpolation and
//!   easing are ADR-0029's transition sampler's; the plan only supplies the ends.
//!
//! - Motion ([ADR-0099](../../docs/adr/0099-native-motion-modifiers-and-timelines.md)):
//!   [`Modifier`]s that wiggle, oscillate or drift an item's properties, [`evaluate`]d as
//!   pure functions of the animation time into [`Offsets`], which [`Offsets::blend`]
//!   mixes across a keyframe move; and [`Timeline`]s of timed [`Cue`]s, whose
//!   [`Timeline::due`] gives each cue's exact fire time on the animation clock.
//! - Audio-reactive modifiers
//!   ([ADR-0100](../../docs/adr/0100-audio-reactive-motion-modifiers.md)): [`Kind::Audio`]
//!   follows a measure of a bus or a source through an [`Envelope`] the evaluator keeps,
//!   and [`evaluate_with`] takes each one's level.
//!
//! The geometry, crop and effects types are type parameters, so this crate doesn't
//! depend on `jstream-protocol`: the graph instantiates it with the protocol's
//! `ItemGeometry` and `Crop`, and its own filter chains.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-anim/README.md#purpose, docs/adr/0049-keyframe-scene-model-and-evaluation.md, docs/adr/0024-scene-structure.md, docs/adr/0088-keyframe-scenes-override-item-effects.md, docs/adr/0099-native-motion-modifiers-and-timelines.md, docs/adr/0100-audio-reactive-motion-modifiers.md

#![forbid(unsafe_code)]

mod audio;
mod keyframe;
mod modifier;
mod switch;
mod timeline;

pub use audio::{Audio, Curve, Envelope, HIGH_HZ, LOW_HZ, MAX_ENVELOPE_MS, MIN_DB, Measure};
pub use keyframe::{
    Field, InvalidValue, Inverse, Keyframe, Override, Props, RestoreConflict, Value,
};
pub use modifier::{
    Clock, InvalidModifier, Kind, MAX_MODIFIERS, Modifier, Offsets, Property, Shape, check_list,
    evaluate, evaluate_with,
};
pub use switch::{Motion, Step, plan_switch};
pub use timeline::{
    Cue, CueAction, Fire, InvalidTimeline, Looping, MAX_CUES, MAX_DURATION_MS, Position, Timeline,
};
