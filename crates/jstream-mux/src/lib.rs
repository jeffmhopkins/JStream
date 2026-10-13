//! Pure-Rust container muxers. This first cut writes FLV, the container RTMP carries, for
//! H.264 and AAC ([`flv`]), with the pieces it needs: AMF0 values ([`amf0`]), H.264
//! Annex B to AVCC framing and the decoder configuration record ([`h264`]), and the AAC
//! `AudioSpecificConfig` ([`aac`]). MP4/fMP4 and enhanced RTMP (HEVC, AV1) come later.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-mux/README.md#purpose, docs/adr/0003-media-stack-and-codecs.md, docs/adr/0009-youtube-rtmps-and-api.md

#![forbid(unsafe_code)]

pub mod aac;
pub mod amf0;
mod error;
pub mod flv;
pub mod h264;

pub use error::MuxError;
