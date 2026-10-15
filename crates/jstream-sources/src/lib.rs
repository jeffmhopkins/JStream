//! Source kinds (ADR-0031, ADR-0025). A [`Registry`] holds every kind by ID: its
//! [`KindDescriptor`](jstream_protocol::KindDescriptor), which the engine reports through
//! `kind.list` and `kind.describe` and validates settings against, and how to create its
//! sources. A [`Source`] produces video frames on request, for snapshots and tests.
//!
//! Built so far: `color` (a solid RGBA colour of a given size), `image` (PNG, JPEG,
//! WebP and GIF, animated GIF and WebP included, decoded with pure-Rust crates) and
//! `text` (a string drawn with a TrueType or OpenType font, read with `skrifa`), and
//! `browser` (a web page rendered off screen by the injected
//! [`BrowserBackend`] from [`Context::browser`], its
//! frames on the GPU and its audio a [`SourceAudio`]), with its `youtube_chat` preset
//! (a live video's public popout chat page, no sign-in). The
//! audio-only kinds (`audio_input`, `desktop_audio` and `app_audio`) make no frames: the
//! engine records the [`AudioTarget`] their settings name. The other built-in kinds are
//! described only. Driving sources from the engine's render
//! thread comes later; the seam is [`Registry::create`] plus [`Source::frame`] and
//! [`Source::next_change`].
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-sources/README.md#purpose, docs/adr/0031-source-types-and-decoding-scope.md, docs/adr/0025-typed-settings-kind-descriptors.md

#![forbid(unsafe_code)]

mod audio;
mod browser;
mod color;
mod describe;
mod error;
mod image;
mod registry;
mod source;
mod text;
mod youtube_chat;

pub use audio::AudioTarget;
pub use error::SourceError;
/// The browser engine seam `browser` sources open their pages through, which the host
/// injects into the engine (ADR-0021).
pub use jstream_browser::BrowserBackend;
pub use registry::Registry;
pub use source::{AssetRoots, Context, Limits, Source, SourceAudio, SourceKind, needs_restart};
pub use youtube_chat::YOUTUBE_CHAT_CSS;
