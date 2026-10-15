//! Source kinds (ADR-0031, ADR-0025). A [`Registry`] holds every kind by ID: its
//! [`KindDescriptor`](jstream_protocol::KindDescriptor), which the engine reports through
//! `kind.list` and `kind.describe` and validates settings against, and how to create its
//! sources. A [`Source`] produces video frames on request, for snapshots and tests.
//!
//! Built so far: `color` (a solid RGBA colour of a given size), `image` (PNG, JPEG,
//! WebP and GIF, animated GIF and WebP included, decoded with pure-Rust crates) and
//! `text` (a string drawn with a TrueType or OpenType font, read with `skrifa`), and
//! `waveform` and `spectrum` (the latest audio of an [`AudioScope`]). The
//! audio-only kinds (`audio_input`, `desktop_audio` and `app_audio`) make no frames: the
//! engine records the [`AudioTarget`] their settings name. The other built-in kinds are
//! described only. Driving sources from the engine's render
//! thread comes later; the seam is [`Registry::create`] plus [`Source::frame`] and
//! [`Source::next_change`].
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-sources/README.md#purpose, docs/adr/0031-source-types-and-decoding-scope.md, docs/adr/0025-typed-settings-kind-descriptors.md

#![forbid(unsafe_code)]

mod audio;
mod color;
mod describe;
mod error;
mod image;
mod registry;
mod scope;
mod source;
mod text;

pub use audio::AudioTarget;
pub use error::SourceError;
pub use registry::Registry;
pub use scope::AudioScope;
pub use source::{AssetRoots, Context, Limits, Painter, Source, SourceKind, needs_restart};
