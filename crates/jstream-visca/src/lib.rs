//! JStream's VISCA format, for PTZ control in both directions
//! (docs/adr/0122-controller-input-midi-and-one-binding-system.md): JStream driving
//! physical PTZ heads (#1182), and a PTZ controller driving JStream's virtual cameras
//! (#1181).
//!
//! Docs: crates/jstream-visca/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/adr/0122-controller-input-midi-and-one-binding-system.md
//!
//! The format only: no sockets, no threads, no allocation and no dependencies. Messages
//! are at most 16 bytes, so they're held in a fixed [`Bytes`].
//!
//! - [`Command`], [`Inquiry`] and [`Request`]: what a controller sends, encoded with
//!   [`Command::encode`] and [`Inquiry::encode`] and parsed with [`Request::parse`].
//! - [`Reply`] and [`InquiryValue`]: what a camera answers, both ways.
//! - [`ip`]: Sony-style VISCA over IP framing, sequence numbers and control messages,
//!   and splitting a raw VISCA byte stream into messages.
//!
//! ```
//! use jstream_visca::{Address, Command, PresetOp, Reply, ip};
//!
//! let cam = Address::new(1).ok_or(jstream_visca::Error::Syntax)?;
//! let recall = Command::Preset(PresetOp::Recall, 4).encode(cam)?;
//! assert_eq!(recall.as_slice(), &[0x81, 0x01, 0x04, 0x3F, 0x02, 0x04, 0xFF]);
//! let mut datagram = [0; 24];
//! let n = ip::command(&mut datagram, 0, &recall)?;
//! assert_eq!(ip::parse(&datagram[..n])?.payload, recall.as_slice());
//! assert_eq!(Reply::parse(&[0x90, 0x41, 0xFF])?, (cam, Reply::Ack { socket: 1 }));
//! # Ok::<(), jstream_visca::Error>(())
//! ```

#![forbid(unsafe_code)]

mod command;
pub mod ip;
mod reply;

use std::fmt;

pub use command::{
    Address, Command, Drive, FocusMode, Inquiry, MAX_PAN_SPEED, MAX_TILT_SPEED, Pan, PositionWidth,
    PresetOp, Request, Tilt,
};
pub use reply::{ErrorKind, InquiryValue, Reply};

/// The longest VISCA message: 16 bytes, terminator included.
pub const MAX_MESSAGE: usize = 16;

/// Why bytes aren't a VISCA message, or a value can't be one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a well-formed VISCA message or datagram. A camera answers this with a syntax
    /// error.
    Syntax,
    /// A well-formed command or inquiry this crate doesn't know. A camera answers it
    /// "not executable".
    Unsupported,
    /// A value out of the range the message can carry: the field named.
    OutOfRange(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax => f.write_str("not a well-formed VISCA message"),
            Self::Unsupported => f.write_str("a VISCA message JStream doesn't support"),
            Self::OutOfRange(what) => write!(f, "VISCA {what} out of range"),
        }
    }
}

impl std::error::Error for Error {}

/// One VISCA message's bytes, at most [`MAX_MESSAGE`], held without allocating.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Bytes {
    buf: [u8; MAX_MESSAGE],
    len: usize,
}

impl Bytes {
    const fn new() -> Self {
        Self {
            buf: [0; MAX_MESSAGE],
            len: 0,
        }
    }

    /// Callers never pass more than a message holds; a byte past the end is dropped.
    fn push(&mut self, b: u8) {
        if let Some(slot) = self.buf.get_mut(self.len) {
            *slot = b;
            self.len += 1;
        }
    }

    fn extend(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.push(b);
        }
    }

    /// The message's bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// How many bytes the message has.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether it has none (never, for an encoded message).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.as_slice().iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{b:02X}")?;
        }
        Ok(())
    }
}
