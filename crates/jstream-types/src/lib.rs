//! Core value types shared by every JStream crate: IDs, math, color, rational time and
//! units. Layer 0: no dependencies on other JStream crates.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-types/README.md#purpose

#![forbid(unsafe_code)]

mod rational;

pub use rational::Rational;
