//! VISCA over IP, as Sony's cameras speak it on UDP port 52381: an 8-byte header, then
//! one VISCA message (or a control message).
//!
//! ```text
//! payload type (2, big-endian)  0100 command, 0110 inquiry, 0111 reply,
//!                               0120 device setting command, 0200 control command,
//!                               0201 control reply
//! payload length (2, big-endian)
//! sequence number (4, big-endian)
//! payload
//! ```
//!
//! A controller numbers each message it sends, and the camera's reply carries the same
//! number. A control command `01` resets the camera's expected number to 0; a control
//! reply `0F 01` says the number was out of sequence, and `0F 02` that the message was
//! malformed.

use crate::{Bytes, Error, MAX_MESSAGE};

/// The UDP port Sony-style VISCA over IP uses.
pub const PORT: u16 = 52381;
const HEADER: usize = 8;

/// The payload type of a VISCA-over-IP datagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadType {
    /// `0100`: a VISCA command.
    Command,
    /// `0110`: a VISCA inquiry.
    Inquiry,
    /// `0111`: a VISCA reply.
    Reply,
    /// `0120`: a VISCA device setting command.
    DeviceSetting,
    /// `0200`: a control command.
    Control,
    /// `0201`: a control reply.
    ControlReply,
}

impl PayloadType {
    const fn code(self) -> u16 {
        match self {
            Self::Command => 0x0100,
            Self::Inquiry => 0x0110,
            Self::Reply => 0x0111,
            Self::DeviceSetting => 0x0120,
            Self::Control => 0x0200,
            Self::ControlReply => 0x0201,
        }
    }

    const fn from_code(c: u16) -> Option<Self> {
        Some(match c {
            0x0100 => Self::Command,
            0x0110 => Self::Inquiry,
            0x0111 => Self::Reply,
            0x0120 => Self::DeviceSetting,
            0x0200 => Self::Control,
            0x0201 => Self::ControlReply,
            _ => return None,
        })
    }
}

/// The control command that resets the sequence number.
pub const CONTROL_RESET: [u8; 1] = [0x01];
/// The control reply for a sequence number out of order.
pub const CONTROL_ERROR_SEQUENCE: [u8; 2] = [0x0F, 0x01];
/// The control reply for a malformed message.
pub const CONTROL_ERROR_MESSAGE: [u8; 2] = [0x0F, 0x02];

/// One datagram, from [`parse`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Datagram<'a> {
    /// What the payload is.
    pub kind: PayloadType,
    /// Its sequence number.
    pub seq: u32,
    /// The payload: a VISCA message, or a control message.
    pub payload: &'a [u8],
}

/// Reads one VISCA-over-IP datagram.
///
/// # Errors
///
/// [`Error::Syntax`] for a datagram shorter than its header, an unknown payload type, a
/// length that disagrees with the datagram, or a payload of 0 or more than 16 bytes.
pub fn parse(datagram: &[u8]) -> Result<Datagram<'_>, Error> {
    if datagram.len() < HEADER {
        return Err(Error::Syntax);
    }
    let kind = PayloadType::from_code(u16::from_be_bytes([datagram[0], datagram[1]]))
        .ok_or(Error::Syntax)?;
    let len = usize::from(u16::from_be_bytes([datagram[2], datagram[3]]));
    let seq = u32::from_be_bytes([datagram[4], datagram[5], datagram[6], datagram[7]]);
    let payload = &datagram[HEADER..];
    if len == 0 || len > MAX_MESSAGE || payload.len() != len {
        return Err(Error::Syntax);
    }
    Ok(Datagram { kind, seq, payload })
}

/// Writes a datagram of `kind` with sequence number `seq` around `payload` into `out`,
/// and gives its length.
///
/// # Errors
///
/// [`Error::OutOfRange`] for an empty payload or one over 16 bytes, or an `out` too
/// small.
pub fn write(out: &mut [u8], kind: PayloadType, seq: u32, payload: &[u8]) -> Result<usize, Error> {
    if payload.is_empty() || payload.len() > MAX_MESSAGE {
        return Err(Error::OutOfRange("payload"));
    }
    let end = HEADER + payload.len();
    let buf = out.get_mut(..end).ok_or(Error::OutOfRange("buffer"))?;
    buf[..2].copy_from_slice(&kind.code().to_be_bytes());
    buf[2..4].copy_from_slice(&(payload.len() as u16).to_be_bytes());
    buf[4..8].copy_from_slice(&seq.to_be_bytes());
    buf[HEADER..].copy_from_slice(payload);
    Ok(end)
}

/// Splits the first VISCA message off a byte stream (raw VISCA over TCP, or a serial
/// line): the bytes up to and including the first `FF`, or `None` until one arrives.
/// A run longer than a message without `FF` is returned whole, for the caller to
/// refuse.
#[must_use]
pub fn next_message(stream: &[u8]) -> Option<&[u8]> {
    match stream.iter().position(|&b| b == 0xFF) {
        Some(i) => Some(&stream[..=i]),
        None if stream.len() > MAX_MESSAGE => Some(&stream[..MAX_MESSAGE]),
        None => None,
    }
}

/// The sequence numbers a controller sends with: from 0, one more each message, back to
/// 0 after a reset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sequence(u32);

impl Sequence {
    /// The number for the next message, then counts it.
    pub fn take(&mut self) -> u32 {
        let n = self.0;
        self.0 = self.0.wrapping_add(1);
        n
    }

    /// Back to 0, as after a [`CONTROL_RESET`].
    pub fn reset(&mut self) {
        self.0 = 0;
    }
}

/// Wraps a message in a command datagram: a convenience for the common case.
///
/// # Errors
///
/// As [`write()`].
pub fn command(out: &mut [u8], seq: u32, message: &Bytes) -> Result<usize, Error> {
    write(out, PayloadType::Command, seq, message.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Address, Command};

    #[test]
    fn a_command_datagram_round_trips() {
        let cam = Address::new(1).unwrap_or(Address::BROADCAST);
        let home = Command::Home.encode(cam).unwrap();
        let mut buf = [0; 32];
        let n = command(&mut buf, 0x0102_0304, &home).unwrap();
        assert_eq!(
            &buf[..n],
            &[
                0x01, 0x00, 0x00, 0x05, 0x01, 0x02, 0x03, 0x04, 0x81, 0x01, 0x06, 0x04, 0xFF
            ]
        );
        assert_eq!(
            parse(&buf[..n]),
            Ok(Datagram {
                kind: PayloadType::Command,
                seq: 0x0102_0304,
                payload: home.as_slice()
            })
        );
    }

    #[test]
    fn control_messages_and_bad_datagrams() {
        let mut buf = [0; 32];
        let n = write(&mut buf, PayloadType::Control, 0, &CONTROL_RESET).unwrap();
        assert_eq!(&buf[..n], &[0x02, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0x01]);
        assert_eq!(parse(&buf[..n - 1]), Err(Error::Syntax));
        assert_eq!(
            parse(&[0x09, 0x99, 0, 1, 0, 0, 0, 0, 1]),
            Err(Error::Syntax)
        );
        assert_eq!(
            write(&mut buf, PayloadType::Command, 0, &[]),
            Err(Error::OutOfRange("payload"))
        );
        assert_eq!(
            write(&mut [0; 8], PayloadType::Command, 0, &[1]),
            Err(Error::OutOfRange("buffer"))
        );
    }

    #[test]
    fn streams_split_at_the_terminator_and_sequences_count() {
        assert_eq!(
            next_message(&[0x81, 0x01, 0xFF, 0x81]),
            Some(&[0x81, 0x01, 0xFF][..])
        );
        assert_eq!(next_message(&[0x81, 0x01]), None);
        assert_eq!(next_message(&[0; 20]).map(<[u8]>::len), Some(16));
        let mut s = Sequence::default();
        assert_eq!((s.take(), s.take()), (0, 1));
        s.reset();
        assert_eq!(s.take(), 0);
    }
}
