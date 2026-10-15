//! The keyframe-scene model ([ADR-0049](../../docs/adr/0049-keyframe-scene-model-and-evaluation.md)):
//! what a variant of a scene (a keyframe, ADR-0024 §2) stores, what its items look like,
//! and what a switch between two keyframes of one scene animates. Layer 0: no
//! dependencies, no `unsafe`, no I/O, so the graph, the engine and a client previewing a
//! keyframe share one definition.
//!
//! - [`Keyframe`] is a variant's override set: per item, an [`Override`] of some of the
//!   four keyable fields ([`Field`]). Writing and clearing a field return the edit that
//!   undoes it ([`Inverse`]); dropping overrides returns what restores them.
//! - [`Keyframe::effective`] computes an item's values in the variant from the
//!   **parent's current values**: an overridden field takes the override, every other
//!   field follows the parent live (ADR-0049 §1).
//! - [`plan_switch`] classifies each item of a switch between two members of a family
//!   (a parent and its variants) and gives its two ends ([`Step`]). Interpolation and
//!   easing are ADR-0029's transition sampler's; the plan only supplies the ends.
//!
//! The geometry and crop types are type parameters, so this crate doesn't depend on
//! `jstream-protocol`: the graph instantiates it with the protocol's `ItemGeometry` and
//! `Crop`.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-anim/README.md#purpose, docs/adr/0049-keyframe-scene-model-and-evaluation.md, docs/adr/0024-scene-structure.md

#![forbid(unsafe_code)]

mod keyframe;
mod switch;

pub use keyframe::{
    Field, InvalidValue, Inverse, Keyframe, Override, Props, RestoreConflict, Value,
};
pub use switch::{Motion, Step, plan_switch};
