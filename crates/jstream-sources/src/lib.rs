//! Source kinds (ADR-0061, ADR-0025). A [`Registry`] holds every kind by ID: its
//! [`KindDescriptor`](jstream_protocol::KindDescriptor), which the engine reports through
//! `kind.list` and `kind.describe` and validates settings against, and how to create its
//! sources. A [`Source`] produces video frames on request, for snapshots and tests.
//!
//! Built so far: `color` (a solid RGBA colour of a given size), `image` (PNG, JPEG,
//! WebP and GIF, animated GIF and WebP included, decoded with pure-Rust crates), `text`
//! (a string drawn with a TrueType or OpenType font, read with `skrifa`), `media` (a
//! video file, demuxed and decoded by the platform's [`MediaDecoders`] from
//! [`Context::media`]), `waveform` and `spectrum` (the latest audio of an
//! [`AudioScope`]: the program mix's, a bus's or another source's, found through
//! [`AudioScopes`]), `browser` (a web page rendered off screen by the injected
//! [`BrowserBackend`] from [`Context::browser`], its frames on the GPU and its audio a
//! [`SourceAudio`]), with its `youtube_chat` preset (a live video's public popout chat
//! page, no sign-in), `chat_message` (the featured live chat message, ADR-0041 §2,
//! which the host sets through the context's [`ChatFeature`]), and `display`, `window`
//! and `webcam` (a webcam or capture card), which say what the platform captures
//! ([`CaptureSpec`]). The audio-only kinds (`audio_input`, `desktop_audio` and
//! `app_audio`) make no frames: the engine records the [`AudioTarget`] their settings
//! name. `jstream-core` drives sources from its `jstream-sources` thread
//! through [`Registry::create`], [`Source::frame`], [`Source::next_change`],
//! [`Source::show`], [`Source::painter`] and [`Source::capture`].
//!
//! [`MediaDecoders`]: jstream_platform::MediaDecoders
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-sources/README.md#purpose, docs/adr/0061-source-types-and-decoding-scope-v2.md, docs/adr/0025-typed-settings-kind-descriptors.md

#![forbid(unsafe_code)]

mod audio;
mod browser;
mod capture;
mod chat;
mod color;
mod describe;
mod error;
mod image;
mod media;
mod registry;
mod scope;
mod source;
mod text;
mod youtube_chat;

pub use audio::AudioTarget;
pub use browser::opens_browser;
pub use capture::{CaptureSpec, webcam_device};
pub use chat::{ChatFeature, FeaturedMessage, MAX_CHAT_LABEL, MAX_CHAT_TEXT};
pub use error::SourceError;
pub use image::{StillImage, decode_still};
/// The browser engine seam `browser` sources open their pages through, which the host
/// injects into the engine (ADR-0021).
pub use jstream_browser::BrowserBackend;
pub use media::MediaAudio;
pub use registry::Registry;
pub use scope::{AudioScope, AudioScopes, drawn_source};
pub use source::{
    AssetRoots, Context, Limits, Painter, Source, SourceAudio, SourceKind, needs_restart,
};
pub use text::MAX_DENSITY;
pub use youtube_chat::YOUTUBE_CHAT_CSS;
