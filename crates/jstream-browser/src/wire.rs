//! The pipe between the engine and the browser helper process: the messages each side
//! sends, and their encoding. Each message is a little-endian `u32` length, then a tag
//! byte and the message's fields: integers little-endian, strings and sample runs
//! length-prefixed. A message over [`MAX_MESSAGE_BYTES`] or with anything malformed in
//! it ends the connection, as the other side can no longer be trusted to be in step.
//!
//! The helper process (`jstream-browser-helper`) holds CEF and its pages; the engine
//! holds a [`BrowserBackend`](crate::BrowserBackend) that drives it. Pages are numbered
//! by the engine. Nothing on the pipe carries a time: the engine stamps frames and audio
//! on its own media clock as they arrive.
//!
//! The conversation:
//! 1. The engine sends [`ToHelper::Start`]; the helper starts CEF and answers
//!    [`ToEngine::Ready`] or [`ToEngine::StartFailed`].
//! 2. [`ToHelper::Open`] is answered by [`ToEngine::Opened`] or [`ToEngine::OpenFailed`].
//!    An open page sends frames, audio and refused-request counts until the engine
//!    closes it.
//! 3. Each [`ToEngine::Frame`] names one of the page's shared textures, which the helper
//!    doesn't paint into again until the engine sends [`ToHelper::Released`] for it.
//! 4. [`ToHelper::Shutdown`], or the pipe closing, closes every page and ends the helper.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use jstream_media::FrameSize;

use crate::{PageLocation, PageSpec};

/// The longest message either side sends: a CSS update at most 64 KiB, an audio packet
/// of at most 4096 stereo frames, or a URL at most 8 KiB.
pub(crate) const MAX_MESSAGE_BYTES: usize = 128 * 1024;

/// The most samples one audio message carries: 4096 stereo frames.
pub(crate) const MAX_AUDIO_SAMPLES: usize = 4096 * 2;

/// What the engine asks of the helper.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ToHelper {
    /// Start CEF, keeping its state under `cache_root` and logging warnings to
    /// `log_file` (or CEF's default). The first message, and only the first.
    Start {
        cache_root: String,
        log_file: Option<String>,
    },
    /// Copy pages' paints onto shared textures of the adapter with this LUID, or keep
    /// no frames (`None`).
    Adapter { luid: Option<u64> },
    /// Open a page.
    Open { page: u32, spec: PageSpec },
    /// Replace a page's custom CSS.
    SetCss { page: u32, css: String },
    /// Close a page.
    Close { page: u32 },
    /// The engine is done with a page's shared texture `slot`: the helper may paint
    /// into it again.
    Released { page: u32, slot: u8 },
    /// Close every page and exit.
    Shutdown,
}

/// What the helper tells the engine.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ToEngine {
    /// CEF started: the helper takes requests now.
    Ready,
    /// CEF couldn't start; the helper exits.
    StartFailed { reason: String },
    /// A page's browser was asked for.
    Opened { page: u32 },
    /// A page couldn't be opened.
    OpenFailed { page: u32, reason: String },
    /// A page painted into its shared texture `slot`, whose NT handle in the helper's
    /// process is `handle` (the same for a slot until the page's size or adapter
    /// changes).
    Frame {
        page: u32,
        slot: u8,
        handle: u64,
        width: u32,
        height: u32,
    },
    /// A packet of a page's audio: interleaved 48 kHz stereo.
    Audio { page: u32, samples: Vec<f32> },
    /// A page's audio stream failed.
    AudioFailed { page: u32, reason: String },
    /// A page's refused requests so far.
    Denied { page: u32, count: u64 },
    /// A page's browser closed.
    Closed { page: u32 },
    /// Something went wrong in the helper that the engine should log: the helper has no
    /// log of its own. The text names no page URL or path.
    Warning { message: String },
}

/// Why a message couldn't be read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WireError {
    /// The pipe failed or closed.
    #[error("the browser helper's pipe: {0}")]
    Io(#[from] io::Error),
    /// The message is malformed.
    #[error("a malformed message from the browser helper's pipe: {0}")]
    Malformed(&'static str),
}

/// Writes one message's body; [`send`] adds the length.
struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn len(&mut self, len: usize) -> Result<(), WireError> {
        let len = u32::try_from(len).map_err(|_| WireError::Malformed("a field is too long"))?;
        self.u32(len);
        Ok(())
    }
    fn str(&mut self, s: &str) -> Result<(), WireError> {
        self.len(s.len())?;
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// Reads one message's body.
struct In<'a>(&'a [u8]);

impl In<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], WireError> {
        if self.0.len() < n {
            return Err(WireError::Malformed("the message ends early"));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn len(&mut self) -> Result<usize, WireError> {
        usize::try_from(self.u32()?).map_err(|_| WireError::Malformed("a field is too long"))
    }
    fn string(&mut self) -> Result<String, WireError> {
        let len = self.len()?;
        String::from_utf8(self.take(len)?.to_vec())
            .map_err(|_| WireError::Malformed("a string isn't UTF-8"))
    }
    fn end(self) -> Result<(), WireError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(WireError::Malformed("the message has bytes left over"))
        }
    }
}

impl ToHelper {
    fn encode(&self, out: &mut Out) -> Result<(), WireError> {
        match self {
            Self::Start {
                cache_root,
                log_file,
            } => {
                out.u8(5);
                out.str(cache_root)?;
                match log_file {
                    Some(file) => {
                        out.u8(1);
                        out.str(file)?;
                    }
                    None => out.u8(0),
                }
            }
            Self::Adapter { luid } => {
                out.u8(0);
                match luid {
                    Some(luid) => {
                        out.u8(1);
                        out.u64(*luid);
                    }
                    None => out.u8(0),
                }
            }
            Self::Open { page, spec } => {
                out.u8(1);
                out.u32(*page);
                match &spec.location {
                    PageLocation::Blank => out.u8(0),
                    PageLocation::Web(url) => {
                        out.u8(1);
                        out.str(url)?;
                    }
                    PageLocation::File(path) => {
                        out.u8(2);
                        let path = path
                            .to_str()
                            .ok_or(WireError::Malformed("the page's file path isn't UTF-8"))?;
                        out.str(path)?;
                    }
                }
                out.u32(spec.size.width());
                out.u32(spec.size.height());
                out.u32(spec.fps);
                out.str(&spec.css)?;
                out.u8(u8::from(spec.local_network));
            }
            Self::SetCss { page, css } => {
                out.u8(2);
                out.u32(*page);
                out.str(css)?;
            }
            Self::Close { page } => {
                out.u8(3);
                out.u32(*page);
            }
            Self::Shutdown => out.u8(4),
            Self::Released { page, slot } => {
                out.u8(6);
                out.u32(*page);
                out.u8(*slot);
            }
        }
        Ok(())
    }

    fn decode(mut body: In<'_>) -> Result<Self, WireError> {
        let message = match body.u8()? {
            0 => Self::Adapter {
                luid: match body.u8()? {
                    0 => None,
                    1 => Some(body.u64()?),
                    _ => return Err(WireError::Malformed("an adapter flag isn't 0 or 1")),
                },
            },
            1 => {
                let page = body.u32()?;
                let location = match body.u8()? {
                    0 => PageLocation::Blank,
                    1 => PageLocation::Web(body.string()?),
                    2 => PageLocation::File(PathBuf::from(body.string()?)),
                    _ => return Err(WireError::Malformed("an unknown page location")),
                };
                let (width, height) = (body.u32()?, body.u32()?);
                let size = FrameSize::new(width, height)
                    .map_err(|_| WireError::Malformed("a page size is invalid"))?;
                let fps = body.u32()?;
                let css = body.string()?;
                let local_network = match body.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(WireError::Malformed("a local-network flag isn't 0 or 1")),
                };
                Self::Open {
                    page,
                    spec: PageSpec {
                        location,
                        size,
                        fps,
                        css,
                        local_network,
                    },
                }
            }
            2 => Self::SetCss {
                page: body.u32()?,
                css: body.string()?,
            },
            3 => Self::Close { page: body.u32()? },
            4 => Self::Shutdown,
            5 => Self::Start {
                cache_root: body.string()?,
                log_file: match body.u8()? {
                    0 => None,
                    1 => Some(body.string()?),
                    _ => return Err(WireError::Malformed("a log-file flag isn't 0 or 1")),
                },
            },
            6 => Self::Released {
                page: body.u32()?,
                slot: body.u8()?,
            },
            _ => return Err(WireError::Malformed("an unknown message to the helper")),
        };
        body.end()?;
        Ok(message)
    }
}

impl ToEngine {
    fn encode(&self, out: &mut Out) -> Result<(), WireError> {
        match self {
            Self::Ready => out.u8(0),
            Self::OpenFailed { page, reason } => {
                out.u8(1);
                out.u32(*page);
                out.str(reason)?;
            }
            Self::Frame {
                page,
                slot,
                handle,
                width,
                height,
            } => {
                out.u8(2);
                out.u32(*page);
                out.u8(*slot);
                out.u64(*handle);
                out.u32(*width);
                out.u32(*height);
            }
            Self::Audio { page, samples } => {
                if samples.len() > MAX_AUDIO_SAMPLES || samples.len() % 2 != 0 {
                    return Err(WireError::Malformed("an audio packet's length is invalid"));
                }
                out.u8(3);
                out.u32(*page);
                out.len(samples.len())?;
                for sample in samples {
                    out.0.extend_from_slice(&sample.to_le_bytes());
                }
            }
            Self::AudioFailed { page, reason } => {
                out.u8(4);
                out.u32(*page);
                out.str(reason)?;
            }
            Self::Denied { page, count } => {
                out.u8(5);
                out.u32(*page);
                out.u64(*count);
            }
            Self::Closed { page } => {
                out.u8(6);
                out.u32(*page);
            }
            Self::StartFailed { reason } => {
                out.u8(7);
                out.str(reason)?;
            }
            Self::Opened { page } => {
                out.u8(8);
                out.u32(*page);
            }
            Self::Warning { message } => {
                out.u8(9);
                out.str(message)?;
            }
        }
        Ok(())
    }

    fn decode(mut body: In<'_>) -> Result<Self, WireError> {
        let message = match body.u8()? {
            0 => Self::Ready,
            1 => Self::OpenFailed {
                page: body.u32()?,
                reason: body.string()?,
            },
            2 => Self::Frame {
                page: body.u32()?,
                slot: body.u8()?,
                handle: body.u64()?,
                width: body.u32()?,
                height: body.u32()?,
            },
            3 => {
                let page = body.u32()?;
                let len = body.len()?;
                if len > MAX_AUDIO_SAMPLES || len % 2 != 0 {
                    return Err(WireError::Malformed("an audio packet's length is invalid"));
                }
                let bytes = body.take(len * 4)?;
                let samples = bytes
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect();
                Self::Audio { page, samples }
            }
            4 => Self::AudioFailed {
                page: body.u32()?,
                reason: body.string()?,
            },
            5 => Self::Denied {
                page: body.u32()?,
                count: body.u64()?,
            },
            6 => Self::Closed { page: body.u32()? },
            7 => Self::StartFailed {
                reason: body.string()?,
            },
            8 => Self::Opened { page: body.u32()? },
            9 => Self::Warning {
                message: body.string()?,
            },
            _ => return Err(WireError::Malformed("an unknown message to the engine")),
        };
        body.end()?;
        Ok(message)
    }
}

/// A message that can cross the pipe.
pub(crate) trait Message: Sized {
    #[doc(hidden)]
    fn encode_body(&self) -> Result<Vec<u8>, WireError>;
    #[doc(hidden)]
    fn decode_body(body: &[u8]) -> Result<Self, WireError>;
}

impl Message for ToHelper {
    fn encode_body(&self) -> Result<Vec<u8>, WireError> {
        let mut out = Out(Vec::new());
        self.encode(&mut out)?;
        Ok(out.0)
    }
    fn decode_body(body: &[u8]) -> Result<Self, WireError> {
        Self::decode(In(body))
    }
}

impl Message for ToEngine {
    fn encode_body(&self) -> Result<Vec<u8>, WireError> {
        let mut out = Out(Vec::new());
        self.encode(&mut out)?;
        Ok(out.0)
    }
    fn decode_body(body: &[u8]) -> Result<Self, WireError> {
        Self::decode(In(body))
    }
}

/// Writes one message to `pipe`.
///
/// # Errors
/// The message is too long or malformed, or the write failed.
pub(crate) fn send(pipe: &mut impl Write, message: &impl Message) -> Result<(), WireError> {
    let body = message.encode_body()?;
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(WireError::Malformed("a message is too long"));
    }
    let len =
        u32::try_from(body.len()).map_err(|_| WireError::Malformed("a message is too long"))?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&body);
    pipe.write_all(&frame)?;
    pipe.flush()?;
    Ok(())
}

/// Reads one message from `pipe`, or `None` when it closed cleanly between messages.
///
/// # Errors
/// The read failed, the pipe closed mid-message, or the message is malformed.
pub(crate) fn receive<M: Message>(pipe: &mut impl Read) -> Result<Option<M>, WireError> {
    let mut len = [0; 4];
    let mut read = 0;
    while read < 4 {
        match pipe.read(&mut len[read..])? {
            0 if read == 0 => return Ok(None),
            0 => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
            n => read += n,
        }
    }
    let len = usize::try_from(u32::from_le_bytes(len))
        .map_err(|_| WireError::Malformed("a message is too long"))?;
    if len > MAX_MESSAGE_BYTES {
        return Err(WireError::Malformed("a message is too long"));
    }
    let mut body = vec![0; len];
    pipe.read_exact(&mut body)?;
    M::decode_body(&body).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<M: Message + PartialEq + std::fmt::Debug>(messages: &[M]) {
        let mut pipe = Vec::new();
        for message in messages {
            send(&mut pipe, message).expect("send");
        }
        let mut reader = pipe.as_slice();
        for message in messages {
            assert_eq!(
                receive::<M>(&mut reader).expect("receive").as_ref(),
                Some(message)
            );
        }
        assert!(receive::<M>(&mut reader).expect("end").is_none());
    }

    fn spec(location: PageLocation) -> PageSpec {
        PageSpec {
            location,
            size: FrameSize::new(1920, 1080).expect("size"),
            fps: 60,
            css: "body { color: red; } /* é */".into(),
            local_network: true,
        }
    }

    #[test]
    fn every_message_round_trips() {
        round_trip(&[
            ToHelper::Start {
                cache_root: r"C:\Users\a\AppData\Local\JStream\cef".into(),
                log_file: Some("cef.log".into()),
            },
            ToHelper::Start {
                cache_root: String::new(),
                log_file: None,
            },
            ToHelper::Adapter {
                luid: Some(0x1234_5678_9abc),
            },
            ToHelper::Adapter { luid: None },
            ToHelper::Open {
                page: 1,
                spec: spec(PageLocation::Blank),
            },
            ToHelper::Open {
                page: 2,
                spec: spec(PageLocation::Web("https://example.com/?a=1".into())),
            },
            ToHelper::Open {
                page: 3,
                spec: spec(PageLocation::File(PathBuf::from("/srv/overlay/index.html"))),
            },
            ToHelper::SetCss {
                page: 3,
                css: "x".repeat(64 * 1024),
            },
            ToHelper::Close { page: 2 },
            ToHelper::Released { page: 3, slot: 2 },
            ToHelper::Shutdown,
        ]);
        round_trip(&[
            ToEngine::Ready,
            ToEngine::StartFailed {
                reason: "no libcef.dll".into(),
            },
            ToEngine::Opened { page: 1 },
            ToEngine::OpenFailed {
                page: 1,
                reason: "no".into(),
            },
            ToEngine::Frame {
                page: 1,
                slot: 2,
                handle: 0x7f0,
                width: 3840,
                height: 2160,
            },
            ToEngine::Audio {
                page: 1,
                samples: (0..MAX_AUDIO_SAMPLES).map(|i| i as f32 / 10.0).collect(),
            },
            ToEngine::AudioFailed {
                page: 1,
                reason: "gone".into(),
            },
            ToEngine::Denied {
                page: 1,
                count: u64::MAX,
            },
            ToEngine::Closed { page: 1 },
            ToEngine::Warning {
                message: "a paint could not be copied".into(),
            },
        ]);
    }

    #[test]
    fn malformed_messages_are_refused() {
        let receive_bytes = |bytes: &[u8]| receive::<ToHelper>(&mut &bytes[..]);
        // Too long, before anything is allocated for it.
        assert!(matches!(
            receive_bytes(&u32::MAX.to_le_bytes()),
            Err(WireError::Malformed(_))
        ));
        // Ends inside the length, or inside the body.
        assert!(matches!(receive_bytes(&[1, 0]), Err(WireError::Io(_))));
        assert!(matches!(
            receive_bytes(&[5, 0, 0, 0, 3]),
            Err(WireError::Io(_))
        ));
        // An unknown tag, a field running past the end, and bytes left over.
        for body in [&[9][..], &[3, 1, 0], &[4, 0]] {
            let mut bytes = u32::try_from(body.len())
                .expect("len")
                .to_le_bytes()
                .to_vec();
            bytes.extend_from_slice(body);
            assert!(
                matches!(receive_bytes(&bytes), Err(WireError::Malformed(_))),
                "{body:?}"
            );
        }
        // A page of zero width.
        let mut body = vec![1, 1, 0, 0, 0, 0];
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&1080u32.to_le_bytes());
        body.extend_from_slice(&60u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        let mut bytes = u32::try_from(body.len())
            .expect("len")
            .to_le_bytes()
            .to_vec();
        bytes.extend_from_slice(&body);
        assert!(matches!(
            receive_bytes(&bytes),
            Err(WireError::Malformed(_))
        ));
        // An odd or oversized audio packet can't be sent.
        let odd = ToEngine::Audio {
            page: 1,
            samples: vec![0.0; 3],
        };
        assert!(send(&mut Vec::new(), &odd).is_err());
        let long = ToEngine::Audio {
            page: 1,
            samples: vec![0.0; MAX_AUDIO_SAMPLES + 2],
        };
        assert!(send(&mut Vec::new(), &long).is_err());
    }
}
