//! Stream outputs. For now these are the RTMP publish client ([`rtmp`]), which sends
//! already-built FLV tag bodies to an ingest server such as YouTube's, over TCP
//! (`rtmp://`) or TLS (`rtmps://`, rustls with the `ring` provider), and reconnects
//! with backoff when the connection drops, and the recording file sink ([`record`]),
//! which writes encoded packets as fragmented MP4. The `Output` trait comes
//! later; the virtual-camera output runs in `jstream-core`'s runtime.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-outputs/README.md#purpose, docs/adr/0051-youtube-without-a-required-oauth-client.md, docs/adr/0031-source-types-and-decoding-scope.md

#![forbid(unsafe_code)]

pub mod record;
pub mod rtmp;
