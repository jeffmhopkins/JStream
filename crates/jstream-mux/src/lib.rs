//! Pure-Rust container muxers for H.264 and AAC: FLV, the container RTMP carries
//! ([`flv`]), and fragmented MP4 for recording ([`fmp4`]), with the pieces they share:
//! AMF0 values ([`amf0`]), H.264 Annex B to AVCC framing and the decoder configuration
//! record ([`h264`]), and the AAC `AudioSpecificConfig` ([`aac`]). Enhanced RTMP (HEVC,
//! AV1) comes later.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-mux/README.md#purpose, docs/adr/0031-source-types-and-decoding-scope.md, docs/adr/0034-youtube-interaction-scope-v2.md

#![forbid(unsafe_code)]

pub mod aac;
pub mod amf0;
mod error;
pub mod flv;
pub mod fmp4;
pub mod h264;

pub use error::MuxError;
