//! Encoder sessions. A session owns one platform encoder (`jstream-platform`'s
//! `VideoEncoder` or `AudioEncoder`) and runs it on its own encode thread. The render or
//! mixer side pushes frames or sample blocks into a bounded queue that never blocks and
//! drops (and counts) when full; the encode thread paces the encoder with `can_accept`
//! and `poll`, and fans each packet out to any number of subscribers, each with its own
//! bounded queue, so a slow output loses packets without stalling the encoder or the
//! other outputs (R-RT-004). [`SessionRegistry`] shares one session between outputs
//! with the same mix and settings. Layer 3: depends only on `jstream-platform`.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-encode/README.md#purpose, crates/jstream-encode/README.md#design, docs/adr/0031-source-types-and-decoding-scope.md, docs/adr/0014-media-clock.md

#![forbid(unsafe_code)]

mod audio;
mod error;
mod fanout;
mod registry;
mod session;
mod video;

pub use audio::{AudioInput, AudioSession};
pub use error::{EncodeError, PushError, StreamEnd};
pub use fanout::{SubscriberStats, Subscription};
pub use registry::{SessionKey, SessionRegistry};
pub use session::{EncoderSettings, SessionOptions, SessionStats, StreamInfo};
pub use video::{VideoInput, VideoSession};

#[cfg(test)]
mod tests;
