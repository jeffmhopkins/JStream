//! A blocking RTMP publish client over any `Read + Write` byte stream.
//!
//! It implements the simple handshake, the chunk stream (header compression, extended
//! timestamps, acknowledgement windows, protocol control) and the NetConnection and
//! NetStream commands needed to publish, then carries caller-built FLV tag bodies as
//! RTMP audio, video and data messages. It never builds media payloads itself.
//!
//! ```no_run
//! use jstream_outputs::rtmp::{RtmpPublisher, RtmpUrl, StreamKey, connect_tcp};
//! # fn run(on_metadata: &[u8], video: &[u8], audio: &[u8]) -> Result<(), jstream_outputs::rtmp::RtmpError> {
//! let url = RtmpUrl::parse("rtmp://a.rtmp.youtube.com/live2")?;
//! let key = StreamKey::new("xxxx-xxxx-xxxx-xxxx")?;
//! let tcp = connect_tcp(&url, Some(std::time::Duration::from_secs(10)))?;
//! let mut session = RtmpPublisher::publish(tcp, &url, &key)?;
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
mod url;

pub use error::RtmpError;
pub use publisher::{PublishOptions, RtmpPublisher, connect_tcp, timestamp_ms};
pub use url::{RtmpUrl, Scheme, StreamKey};
