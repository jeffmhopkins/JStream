//! Media value types shared by capture, render, audio, encode and output code: timestamps
//! and time bases, pixel formats, CPU video frames, audio buffers and encoded packets.
//! Layer 0: plain data and invariants only, with no I/O, GPU or platform code, and no
//! dependencies on other JStream crates.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-media/README.md#purpose, docs/adr/0003-media-stack-and-codecs.md, docs/adr/0014-media-clock.md

#![forbid(unsafe_code)]

mod audio;
mod color;
mod error;
mod packet;
mod pixel;
mod time;
mod video;

pub use audio::{AudioBuffer, AudioFormat, ChannelLayout, SampleFormat};
pub use color::{ColorInfo, ColorRange, ColorSpace};
pub use error::MediaError;
pub use packet::{Codec, MediaKind, Packet};
pub use pixel::{ChromaSubsampling, FrameSize, PixelFormat, PlaneGeometry};
pub use time::{MediaTime, Rounding, Timebase};
pub use video::{Plane, VideoFrame, VideoFrameInfo};
