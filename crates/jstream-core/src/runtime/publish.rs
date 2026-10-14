//! Where an output's FLV tags go. Tests record them in memory. A host that is
//! actually live uses [`connect_rtmp`].

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jstream_mux::flv::{FlvTag, TagType};
use jstream_outputs::rtmp::{RtmpError, RtmpPublisher, RtmpUrl, StreamKey, Transport, connect};

use crate::lock;

/// A failure while opening or writing a live publish session.
#[derive(Debug)]
pub struct PublishError {
    reason: String,
}

impl PublishError {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// The error text. Stream keys are not included by this type; RTMP rejections
    /// redact them before they become this string (R-SEC-001).
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for PublishError {}

fn rtmp_err(err: RtmpError) -> PublishError {
    PublishError::new(err.to_string())
}

/// Accepts the FLV tags of one live output. Called on the output thread only.
pub trait LivePublish: Send {
    /// Sends one tag.
    ///
    /// # Errors
    /// When the destination refuses or drops the tag.
    fn send_tag(&mut self, tag: &FlvTag) -> Result<(), PublishError>;

    /// Reads anything the destination has sent since the last tag. Recording
    /// destinations do nothing.
    ///
    /// # Errors
    /// When the destination reports a failure.
    fn poll(&mut self) -> Result<(), PublishError>;

    /// Ends the session.
    ///
    /// # Errors
    /// When the destination does not close cleanly.
    fn finish(&mut self) -> Result<(), PublishError>;
}

/// Keeps every tag in memory, in send order. Clones share the buffer, so a test
/// can read tags after the publish target has moved onto the output thread.
#[derive(Clone, Default)]
pub struct RecordingPublish {
    tags: Arc<Mutex<Vec<FlvTag>>>,
}

impl RecordingPublish {
    /// An empty recording.
    pub fn new() -> Self {
        Self::default()
    }

    /// The tags sent so far, cloned out of the buffer.
    pub fn tags(&self) -> Vec<FlvTag> {
        lock(&self.tags).clone()
    }
}

impl LivePublish for RecordingPublish {
    fn send_tag(&mut self, tag: &FlvTag) -> Result<(), PublishError> {
        lock(&self.tags).push(tag.clone());
        Ok(())
    }

    fn poll(&mut self) -> Result<(), PublishError> {
        Ok(())
    }

    fn finish(&mut self) -> Result<(), PublishError> {
        Ok(())
    }
}

/// An RTMP or RTMPS publish session. `send_metadata` on the publisher adds
/// `@setDataFrame` itself, so script tags are the file-style `onMetaData` body.
pub struct RtmpPublish {
    inner: Option<RtmpPublisher<Transport>>,
}

impl RtmpPublish {
    fn publisher(&mut self) -> Result<&mut RtmpPublisher<Transport>, PublishError> {
        self.inner
            .as_mut()
            .ok_or_else(|| PublishError::new("the RTMP session has finished"))
    }
}

impl LivePublish for RtmpPublish {
    fn send_tag(&mut self, tag: &FlvTag) -> Result<(), PublishError> {
        let publisher = self.publisher()?;
        let result = match tag.tag_type {
            TagType::Script => publisher.send_metadata(&tag.body),
            TagType::Video => publisher.send_video(tag.timestamp, &tag.body),
            TagType::Audio => publisher.send_audio(tag.timestamp, &tag.body),
        };
        result.map_err(rtmp_err)
    }

    fn poll(&mut self) -> Result<(), PublishError> {
        self.publisher()?.poll_incoming().map_err(rtmp_err)
    }

    fn finish(&mut self) -> Result<(), PublishError> {
        match self.inner.take() {
            Some(publisher) => publisher.finish().map(|_| ()).map_err(rtmp_err),
            None => Ok(()),
        }
    }
}

/// Connects to `url` (`rtmp://` or `rtmps://`) and publishes `key`.
///
/// The socket's read timeout is short so the output thread can poll server
/// messages without blocking the pipeline. This opens a network connection;
/// unit tests do not call it (R-TEST-004).
///
/// # Errors
/// When the URL or key is rejected, or the server does not accept the publish.
pub fn connect_rtmp(url: &str, key: &str) -> Result<RtmpPublish, PublishError> {
    let url = RtmpUrl::parse(url).map_err(rtmp_err)?;
    let key = StreamKey::new(key).map_err(rtmp_err)?;
    let transport = connect(&url, Some(Duration::from_secs(10))).map_err(rtmp_err)?;
    transport
        .set_read_timeout(Some(Duration::from_millis(1)))
        .map_err(|err| PublishError::new(format!("setting the RTMP read timeout: {err}")))?;
    transport
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|err| PublishError::new(format!("setting the RTMP write timeout: {err}")))?;
    let publisher = RtmpPublisher::publish(transport, &url, &key).map_err(rtmp_err)?;
    Ok(RtmpPublish {
        inner: Some(publisher),
    })
}
