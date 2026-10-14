//! A blocking RTMP publish client over any `Read + Write` byte stream, with a
//! [`connect`] that opens one over TCP (`rtmp://`) or TLS (`rtmps://`).
//!
//! It implements the simple handshake, the chunk stream (header compression, extended
//! timestamps, acknowledgement windows, protocol control) and the NetConnection and
//! NetStream commands needed to publish, then carries caller-built FLV tag bodies as
//! RTMP audio, video and data messages. It never builds media payloads itself.
//!
//! ```no_run
//! use jstream_outputs::rtmp::{RtmpPublisher, RtmpUrl, StreamKey, connect};
//! # fn run(on_metadata: &[u8], video: &[u8], audio: &[u8]) -> Result<(), jstream_outputs::rtmp::RtmpError> {
//! // rtmps:// connects over TLS, rtmp:// over plain TCP.
//! let url = RtmpUrl::parse("rtmps://a.rtmps.youtube.com/live2")?;
//! let key = StreamKey::new("xxxx-xxxx-xxxx-xxxx")?;
//! let transport = connect(&url, Some(std::time::Duration::from_secs(10)))?;
//! let mut session = RtmpPublisher::publish(transport, &url, &key)?;
//! session.send_metadata(on_metadata)?;
//! session.send_video(0, video)?;
//! session.send_audio(0, audio)?;
//! session.finish()?;
//! # Ok(())
//! # }
//! ```
//!
//! Docs: crates/jstream-outputs/README.md#purpose

mod chunk;
mod error;
mod handshake;
mod publisher;
#[cfg(test)]
mod test_util;
mod transport;
mod url;

pub use error::RtmpError;
pub use publisher::{PublishOptions, RtmpPublisher, timestamp_ms};
pub use transport::{Transport, connect};
pub use url::{RtmpUrl, Scheme, StreamKey};
