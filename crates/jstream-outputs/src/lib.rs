//! Stream outputs. For now this is the RTMP publish client ([`rtmp`]), which sends
//! already-built FLV tag bodies to an ingest server such as YouTube's. The `Output`
//! trait, reconnect with backoff, recording and the virtual-camera output come later.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-outputs/README.md#purpose, docs/adr/0009-youtube-rtmps-and-api.md, docs/adr/0003-media-stack-and-codecs.md

#![forbid(unsafe_code)]

pub mod rtmp;
