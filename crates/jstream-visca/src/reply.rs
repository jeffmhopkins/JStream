//! VISCA replies: what a camera answers.
//!
//! A command is answered with an ACK (`y0 4z FF`, `z` the socket that holds it), then a
//! completion (`y0 5z FF`) when it is done, or an error (`y0 6z ee FF`). An inquiry is
//! answered with a completion that carries data (`y0 50 … FF`), which
//! [`InquiryValue::decode`] reads for the inquiry that was asked.

use crate::command::{
    Address, FocusMode, Inquiry, PositionWidth, fits, get_nibbles, put_nibbles, signed,
};
use crate::{Bytes, Error};

/// An error a camera reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// `01`: message length error.
    MessageLength,
    /// `02`: syntax error.
    Syntax,
    /// `03`: the command buffers are full.
    BufferFull,
    /// `04`: the command was cancelled.
    Cancelled,
    /// `05`: no such socket (for a cancel).
    NoSocket,
    /// `41`: the command can't be done now.
    NotExecutable,
    /// Any other code.
    Other(u8),
}

impl ErrorKind {
    const fn code(self) -> u8 {
        match self {
            Self::MessageLength => 0x01,
            Self::Syntax => 0x02,
            Self::BufferFull => 0x03,
            Self::Cancelled => 0x04,
            Self::NoSocket => 0x05,
            Self::NotExecutable => 0x41,
            Self::Other(c) => c,
        }
    }

    const fn from_code(c: u8) -> Self {
        match c {
            0x01 => Self::MessageLength,
            0x02 => Self::Syntax,
            0x03 => Self::BufferFull,
            0x04 => Self::Cancelled,
            0x05 => Self::NoSocket,
            0x41 => Self::NotExecutable,
            c => Self::Other(c),
        }
    }
}

/// A reply from a camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply<'a> {
    /// `y0 4z FF`: the command was taken into socket `z` (1 or 2).
    Ack {
        /// The socket.
        socket: u8,
    },
    /// `y0 5z FF` with `z` 1 or 2: the command in socket `z` is done.
    Completion {
        /// The socket.
        socket: u8,
    },
    /// `y0 50 … FF`: an inquiry's answer, its bytes between `50` and `FF`.
    Data(&'a [u8]),
    /// `y0 6z ee FF`: an error, for socket `z` (0 when no socket was involved).
    Error {
        /// The socket.
        socket: u8,
        /// What went wrong.
        kind: ErrorKind,
    },
}

impl<'a> Reply<'a> {
    /// Parses one reply and the address it came from.
    ///
    /// # Errors
    ///
    /// [`Error::Syntax`] for bytes that aren't a VISCA reply, including any longer than
    /// [`MAX_MESSAGE`](crate::MAX_MESSAGE).
    pub fn parse(bytes: &'a [u8]) -> Result<(Address, Self), Error> {
        if bytes.len() > crate::MAX_MESSAGE {
            return Err(Error::Syntax);
        }
        let [first, body @ .., 0xFF] = bytes else {
            return Err(Error::Syntax);
        };
        let from = Address::new((first >> 4).wrapping_sub(8)).ok_or(Error::Syntax)?;
        if first & 0x0F != 0 || body.contains(&0xFF) {
            return Err(Error::Syntax);
        }
        let reply = match body {
            [b] if b & 0xF0 == 0x40 => Self::Ack { socket: b & 0x0F },
            [b] if b & 0xF0 == 0x50 && b & 0x0F != 0 => Self::Completion { socket: b & 0x0F },
            [0x50, data @ ..] => Self::Data(data),
            [b, code] if b & 0xF0 == 0x60 => Self::Error {
                socket: b & 0x0F,
                kind: ErrorKind::from_code(*code),
            },
            _ => return Err(Error::Syntax),
        };
        Ok((from, reply))
    }

    /// The reply's bytes, from camera `from`.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfRange`] for the broadcast address (a camera replies from its own
    /// address, never from the one a broadcast was sent to), a socket above 15, data too
    /// long for a message or containing `FF`, or an error code of `FF`.
    pub fn encode(&self, from: Address) -> Result<Bytes, Error> {
        if from == Address::BROADCAST {
            return Err(Error::OutOfRange("reply address"));
        }
        if let Self::Error { kind, .. } = self
            && kind.code() == 0xFF
        {
            return Err(Error::OutOfRange("error code"));
        }
        let mut b = Bytes::new();
        b.push(from.reply_byte());
        match *self {
            Self::Ack { socket } | Self::Completion { socket } | Self::Error { socket, .. }
                if socket > 0x0F =>
            {
                return Err(Error::OutOfRange("socket"));
            }
            Self::Ack { socket } => b.push(0x40 | socket),
            Self::Completion { socket } => b.push(0x50 | socket),
            Self::Data(data) => {
                if data.len() > crate::MAX_MESSAGE - 3 || data.contains(&0xFF) {
                    return Err(Error::OutOfRange("inquiry data"));
                }
                b.push(0x50);
                b.extend(data);
            }
            Self::Error { socket, kind } => b.extend(&[0x60 | socket, kind.code()]),
        }
        b.push(0xFF);
        Ok(b)
    }
}

/// An inquiry's answer, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InquiryValue {
    /// Power on (`02`) or standby (`03`).
    Power(bool),
    /// The zoom position.
    ZoomPosition(u16),
    /// The focus position.
    FocusPosition(u16),
    /// The focus mode (auto `02` or manual `03`).
    FocusMode(FocusMode),
    /// The pan and tilt position, in the camera's signed counts.
    PanTiltPosition {
        /// Pan.
        pan: i32,
        /// Tilt.
        tilt: i32,
        /// How many nibbles the pan took.
        width: PositionWidth,
    },
}

impl InquiryValue {
    /// Reads the data of a [`Reply::Data`] answering `inquiry`.
    ///
    /// # Errors
    ///
    /// [`Error::Syntax`] when the data isn't that inquiry's answer.
    pub fn decode(inquiry: Inquiry, data: &[u8]) -> Result<Self, Error> {
        let nib = |d: &[u8]| get_nibbles(d).ok_or(Error::Syntax);
        Ok(match (inquiry, data) {
            (Inquiry::Power, [0x02]) => Self::Power(true),
            (Inquiry::Power, [0x03]) => Self::Power(false),
            (Inquiry::ZoomPosition, d) if d.len() == 4 => Self::ZoomPosition(nib(d)? as u16),
            (Inquiry::FocusPosition, d) if d.len() == 4 => Self::FocusPosition(nib(d)? as u16),
            (Inquiry::FocusMode, [0x02]) => Self::FocusMode(FocusMode::Auto),
            (Inquiry::FocusMode, [0x03]) => Self::FocusMode(FocusMode::Manual),
            (Inquiry::PanTiltPosition, d) if d.len() == 9 || d.len() == 8 => {
                let (n, width) = if d.len() == 9 {
                    (5, PositionWidth::Pan5Tilt4)
                } else {
                    (4, PositionWidth::Pan4Tilt4)
                };
                Self::PanTiltPosition {
                    pan: signed(nib(&d[..n])?, n),
                    tilt: signed(nib(&d[n..])?, 4),
                    width,
                }
            }
            _ => return Err(Error::Syntax),
        })
    }

    /// The whole reply carrying this value, from camera `from`: what JStream's virtual
    /// camera answers an inquiry with.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfRange`] for a position that doesn't fit its width, or the toggle
    /// focus mode, which isn't a state.
    pub fn encode(&self, from: Address) -> Result<Bytes, Error> {
        let mut data = Bytes::new();
        match *self {
            Self::Power(on) => data.push(if on { 0x02 } else { 0x03 }),
            Self::ZoomPosition(v) | Self::FocusPosition(v) => {
                put_nibbles(&mut data, i64::from(v), 4);
            }
            Self::FocusMode(FocusMode::Auto) => data.push(0x02),
            Self::FocusMode(FocusMode::Manual) => data.push(0x03),
            Self::FocusMode(FocusMode::Toggle) => return Err(Error::OutOfRange("focus mode")),
            Self::PanTiltPosition { pan, tilt, width } => {
                let n = if width == PositionWidth::Pan5Tilt4 {
                    5
                } else {
                    4
                };
                if !fits(pan, n) || !fits(tilt, 4) {
                    return Err(Error::OutOfRange("position"));
                }
                put_nibbles(&mut data, i64::from(pan), n);
                put_nibbles(&mut data, i64::from(tilt), 4);
            }
        }
        Reply::Data(data.as_slice()).encode(from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_what_it_could_not_write_back() {
        // Inquiry data longer than a message: found by the review, the fuzz target's
        // round trip would panic on it.
        let mut long = vec![0x90, 0x50];
        long.extend([0; 14]);
        long.push(0xFF);
        assert_eq!(Reply::parse(&long), Err(Error::Syntax));
        assert_eq!(
            Reply::Ack { socket: 1 }.encode(Address::BROADCAST),
            Err(Error::OutOfRange("reply address"))
        );
        let bad = Reply::Error {
            socket: 1,
            kind: ErrorKind::Other(0xFF),
        };
        assert!(bad.encode(cam1()).is_err());
    }

    fn cam1() -> Address {
        Address::new(1).unwrap_or(Address::BROADCAST)
    }

    #[test]
    fn acks_completions_and_errors_round_trip() {
        let cases: [(Reply<'_>, &[u8]); 4] = [
            (Reply::Ack { socket: 1 }, &[0x90, 0x41, 0xFF]),
            (Reply::Completion { socket: 2 }, &[0x90, 0x52, 0xFF]),
            (
                Reply::Error {
                    socket: 0,
                    kind: ErrorKind::Syntax,
                },
                &[0x90, 0x60, 0x02, 0xFF],
            ),
            (
                Reply::Error {
                    socket: 1,
                    kind: ErrorKind::NotExecutable,
                },
                &[0x90, 0x61, 0x41, 0xFF],
            ),
        ];
        for (r, bytes) in cases {
            assert_eq!(r.encode(cam1()).unwrap().as_slice(), bytes);
            assert_eq!(Reply::parse(bytes), Ok((cam1(), r)));
        }
        let from7 = Address::new(7).unwrap_or(Address::BROADCAST);
        assert_eq!(
            Reply::parse(&[0xF0, 0x41, 0xFF]),
            Ok((from7, Reply::Ack { socket: 1 }))
        );
        assert_eq!(Reply::parse(&[0x81, 0x41, 0xFF]), Err(Error::Syntax));
        assert_eq!(Reply::parse(&[0x90, 0x41]), Err(Error::Syntax));
    }

    #[test]
    fn inquiry_answers_decode_and_encode() {
        let pos = [
            0x90, 0x50, 0x0F, 0x0F, 0x0F, 0x0E, 0x0C, 0x00, 0x01, 0x00, 0x00, 0xFF,
        ];
        let (_, Reply::Data(d)) = Reply::parse(&pos).unwrap() else {
            panic!("not data");
        };
        let v = InquiryValue::decode(Inquiry::PanTiltPosition, d).unwrap();
        assert_eq!(
            v,
            InquiryValue::PanTiltPosition {
                pan: -0x14,
                tilt: 0x100,
                width: PositionWidth::Pan5Tilt4
            }
        );
        assert_eq!(v.encode(cam1()).unwrap().as_slice(), &pos);
        let zoom = InquiryValue::ZoomPosition(0x4000);
        let b = zoom.encode(cam1()).unwrap();
        assert_eq!(b.as_slice(), &[0x90, 0x50, 0x04, 0x00, 0x00, 0x00, 0xFF]);
        assert_eq!(
            InquiryValue::decode(Inquiry::ZoomPosition, &b.as_slice()[2..6]),
            Ok(zoom)
        );
        assert_eq!(
            InquiryValue::decode(Inquiry::Power, &[0x02]),
            Ok(InquiryValue::Power(true))
        );
        assert_eq!(
            InquiryValue::decode(Inquiry::Power, &[0x04]),
            Err(Error::Syntax)
        );
        assert_eq!(
            InquiryValue::decode(Inquiry::ZoomPosition, &[0x10, 0, 0, 0]),
            Err(Error::Syntax)
        );
    }
}
