//! Pure-Rust container muxers: FLV, the container RTMP carries ([`flv`]), with H.264 and
//! AAC, and HEVC and AV1 through enhanced RTMP; and fragmented MP4 for recording
//! ([`fmp4`]), with H.264 and AAC. Also the pieces they share: AMF0 values ([`amf0`]),
//! H.264 Annex B to AVCC framing and the decoder configuration record ([`h264`]), the
//! HEVC record ([`hevc`]), AV1 OBUs and their record ([`av1`]), and the AAC
//! `AudioSpecificConfig` ([`aac`]).
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-mux/README.md#purpose, docs/adr/0031-source-types-and-decoding-scope.md, docs/adr/0041-youtube-interaction-scope-v3.md

#![forbid(unsafe_code)]

pub mod aac;
pub mod amf0;
pub mod av1;
mod bits;
mod error;
pub mod flv;
pub mod fmp4;
pub mod h264;
pub mod hevc;

pub use error::MuxError;
