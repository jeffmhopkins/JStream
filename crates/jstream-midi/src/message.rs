//! MIDI 1.0 messages: channel voice, system common and system real-time.

use std::fmt;

/// A MIDI channel, 0 to 15 on the wire. People and the UI count them 1 to 16, which is
/// what [`fmt::Display`] shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Channel(u8);

impl Channel {
    /// The channel for a wire value, or `None` above 15.
    #[must_use]
    pub const fn new(wire: u8) -> Option<Self> {
        if wire < 16 { Some(Self(wire)) } else { None }
    }

    /// The channel's wire value, 0 to 15.
    #[must_use]
    pub const fn wire(self) -> u8 {
        self.0
    }

    /// The channel as people count it, 1 to 16.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0 + 1
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.number())
    }
}

/// One MIDI 1.0 message other than SysEx.
///
/// A note-on with velocity 0 is parsed as a [`Message::NoteOff`] with velocity 0, as the
/// MIDI specification says it means, so a binding on note-offs sees both spellings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Message {
    /// `8n kk vv`: a key released.
    NoteOff {
        /// The channel.
        channel: Channel,
        /// The key, 0 to 127 (60 is middle C).
        key: u8,
        /// The release velocity, 0 to 127.
        velocity: u8,
    },
    /// `9n kk vv` with `vv` above 0: a key pressed.
    NoteOn {
        /// The channel.
        channel: Channel,
        /// The key, 0 to 127.
        key: u8,
        /// The velocity, 1 to 127.
        velocity: u8,
    },
    /// `An kk pp`: polyphonic key pressure (aftertouch).
    PolyPressure {
        /// The channel.
        channel: Channel,
        /// The key, 0 to 127.
        key: u8,
        /// The pressure, 0 to 127.
        pressure: u8,
    },
    /// `Bn cc vv`: a control change, including the channel mode messages (120 to 127).
    ControlChange {
        /// The channel.
        channel: Channel,
        /// The controller number, 0 to 127.
        controller: u8,
        /// The value, 0 to 127.
        value: u8,
    },
    /// `Cn pp`: a program change.
    ProgramChange {
        /// The channel.
        channel: Channel,
        /// The program, 0 to 127 (shown as 1 to 128 by most devices).
        program: u8,
    },
    /// `Dn pp`: channel pressure (aftertouch).
    ChannelPressure {
        /// The channel.
        channel: Channel,
        /// The pressure, 0 to 127.
        pressure: u8,
    },
    /// `En ll mm`: pitch bend.
    PitchBend {
        /// The channel.
        channel: Channel,
        /// The bend, -8192 to 8191, 0 at the centre.
        value: i16,
    },
    /// `F1 0p vvvv`: a MIDI time code quarter frame.
    TimeCodeQuarterFrame {
        /// Which piece of the time code, 0 to 7.
        piece: u8,
        /// Its four-bit value.
        value: u8,
    },
    /// `F2 ll mm`: song position pointer, in MIDI beats (sixteenth notes, six clocks
    /// each) since the start of the song.
    SongPosition {
        /// Sixteenth notes since the start, 0 to 16383.
        sixteenths: u16,
    },
    /// `F3 ss`: song select.
    SongSelect {
        /// The song, 0 to 127.
        song: u8,
    },
    /// `F6`: tune request.
    TuneRequest,
    /// `F8`: timing clock, 24 per quarter note.
    TimingClock,
    /// `FA`: start from the beginning of the song, on the next clock.
    Start,
    /// `FB`: continue from the current song position, on the next clock.
    Continue,
    /// `FC`: stop, holding the song position.
    Stop,
    /// `FE`: active sensing.
    ActiveSensing,
    /// `FF`: system reset.
    Reset,
}

/// Why bytes aren't one [`Message`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// No bytes at all.
    Empty,
    /// The first byte is a data byte (below `80`), as with running status, which this
    /// model doesn't keep: transports deliver whole messages.
    NoStatus(u8),
    /// `F0` or `F7`: SysEx, which transports carry in their own framing.
    SysEx,
    /// A status byte the MIDI 1.0 specification leaves undefined (`F4`, `F5`, `F9`, `FD`).
    Undefined(u8),
    /// The message has the wrong number of bytes for its status.
    Length {
        /// What the status byte needs.
        expected: usize,
        /// What was given.
        got: usize,
    },
    /// A data byte has its top bit set.
    DataByte(u8),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("no bytes"),
            Self::NoStatus(b) => write!(f, "{b:#04x} is a data byte, not a status byte"),
            Self::SysEx => f.write_str("SysEx is carried in the transport's own framing"),
            Self::Undefined(b) => write!(f, "{b:#04x} is an undefined status byte"),
            Self::Length { expected, got } => {
                write!(f, "the status needs {expected} bytes, not {got}")
            }
            Self::DataByte(b) => write!(f, "data byte {b:#04x} has its top bit set"),
        }
    }
}

impl std::error::Error for ParseError {}

/// The length in bytes of the message a status byte starts, data bytes included: 1 to 3.
/// It is 0 for a data byte (below `80`), for SysEx's `F0` and `F7`, and for the undefined
/// `F4`, `F5`, `F9` and `FD`.
#[must_use]
pub const fn message_len(status: u8) -> usize {
    match status {
        0x00..=0x7F | 0xF0 | 0xF7 | 0xF4 | 0xF5 | 0xF9 | 0xFD => 0,
        0x80..=0xEF => match status & 0xF0 {
            0xC0 | 0xD0 => 2,
            _ => 3,
        },
        0xF2 => 3,
        0xF1 | 0xF3 => 2,
        _ => 1,
    }
}

impl Message {
    /// Parses one whole message: a status byte and exactly the data bytes it takes.
    ///
    /// # Errors
    ///
    /// A [`ParseError`] naming what is wrong with the bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let Some(&status) = bytes.first() else {
            return Err(ParseError::Empty);
        };
        match status {
            0x00..=0x7F => return Err(ParseError::NoStatus(status)),
            0xF0 | 0xF7 => return Err(ParseError::SysEx),
            0xF4 | 0xF5 | 0xF9 | 0xFD => return Err(ParseError::Undefined(status)),
            _ => {}
        }
        let expected = message_len(status);
        if bytes.len() != expected {
            return Err(ParseError::Length {
                expected,
                got: bytes.len(),
            });
        }
        if let Some(&bad) = bytes[1..].iter().find(|&&b| b & 0x80 != 0) {
            return Err(ParseError::DataByte(bad));
        }
        let d1 = bytes.get(1).copied().unwrap_or(0);
        let d2 = bytes.get(2).copied().unwrap_or(0);
        let channel = Channel(status & 0x0F);
        Ok(match status {
            0x80..=0x8F => Self::NoteOff {
                channel,
                key: d1,
                velocity: d2,
            },
            0x90..=0x9F if d2 == 0 => Self::NoteOff {
                channel,
                key: d1,
                velocity: 0,
            },
            0x90..=0x9F => Self::NoteOn {
                channel,
                key: d1,
                velocity: d2,
            },
            0xA0..=0xAF => Self::PolyPressure {
                channel,
                key: d1,
                pressure: d2,
            },
            0xB0..=0xBF => Self::ControlChange {
                channel,
                controller: d1,
                value: d2,
            },
            0xC0..=0xCF => Self::ProgramChange {
                channel,
                program: d1,
            },
            0xD0..=0xDF => Self::ChannelPressure {
                channel,
                pressure: d1,
            },
            0xE0..=0xEF => Self::PitchBend {
                channel,
                value: ((u16::from(d2) << 7 | u16::from(d1)) as i16) - 8192,
            },
            0xF1 => Self::TimeCodeQuarterFrame {
                piece: d1 >> 4,
                value: d1 & 0x0F,
            },
            0xF2 => Self::SongPosition {
                sixteenths: u16::from(d2) << 7 | u16::from(d1),
            },
            0xF3 => Self::SongSelect { song: d1 },
            0xF6 => Self::TuneRequest,
            0xF8 => Self::TimingClock,
            0xFA => Self::Start,
            0xFB => Self::Continue,
            0xFC => Self::Stop,
            0xFE => Self::ActiveSensing,
            // 0xFF, the only status left.
            _ => Self::Reset,
        })
    }

    /// The message's bytes: the first [`len`](Self::len) of the array. A note-off is
    /// written as `8n`, whichever way it was received. Values out of range are masked to
    /// seven bits (pitch bend is held to its range first).
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 3] {
        let ch = |c: &Channel, kind: u8| kind | c.0;
        match *self {
            Self::NoteOff {
                channel,
                key,
                velocity,
            } => [ch(&channel, 0x80), key & 0x7F, velocity & 0x7F],
            Self::NoteOn {
                channel,
                key,
                velocity,
            } => [ch(&channel, 0x90), key & 0x7F, velocity & 0x7F],
            Self::PolyPressure {
                channel,
                key,
                pressure,
            } => [ch(&channel, 0xA0), key & 0x7F, pressure & 0x7F],
            Self::ControlChange {
                channel,
                controller,
                value,
            } => [ch(&channel, 0xB0), controller & 0x7F, value & 0x7F],
            Self::ProgramChange { channel, program } => [ch(&channel, 0xC0), program & 0x7F, 0],
            Self::ChannelPressure { channel, pressure } => [ch(&channel, 0xD0), pressure & 0x7F, 0],
            Self::PitchBend { channel, value } => {
                let raw = (i32::from(value).clamp(-8192, 8191) + 8192) as u16;
                [ch(&channel, 0xE0), (raw & 0x7F) as u8, (raw >> 7) as u8]
            }
            Self::TimeCodeQuarterFrame { piece, value } => {
                [0xF1, (piece & 0x07) << 4 | (value & 0x0F), 0]
            }
            Self::SongPosition { sixteenths } => [
                0xF2,
                (sixteenths & 0x7F) as u8,
                (sixteenths >> 7 & 0x7F) as u8,
            ],
            Self::SongSelect { song } => [0xF3, song & 0x7F, 0],
            Self::TuneRequest => [0xF6, 0, 0],
            Self::TimingClock => [0xF8, 0, 0],
            Self::Start => [0xFA, 0, 0],
            Self::Continue => [0xFB, 0, 0],
            Self::Stop => [0xFC, 0, 0],
            Self::ActiveSensing => [0xFE, 0, 0],
            Self::Reset => [0xFF, 0, 0],
        }
    }

    /// How many of [`to_bytes`](Self::to_bytes)'s bytes the message takes: 1 to 3.
    #[must_use]
    pub fn len(&self) -> usize {
        message_len(self.to_bytes()[0])
    }

    /// Always false: every message has at least its status byte. For clippy's
    /// `len_without_is_empty`.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// The channel of a channel voice message, `None` for a system message.
    #[must_use]
    pub const fn channel(&self) -> Option<Channel> {
        match *self {
            Self::NoteOff { channel, .. }
            | Self::NoteOn { channel, .. }
            | Self::PolyPressure { channel, .. }
            | Self::ControlChange { channel, .. }
            | Self::ProgramChange { channel, .. }
            | Self::ChannelPressure { channel, .. }
            | Self::PitchBend { channel, .. } => Some(channel),
            _ => None,
        }
    }

    /// Whether this is a system real-time message (`F8` to `FF`), which may arrive
    /// between any two others and carries the clock and transport.
    #[must_use]
    pub const fn is_real_time(&self) -> bool {
        matches!(
            self,
            Self::TimingClock
                | Self::Start
                | Self::Continue
                | Self::Stop
                | Self::ActiveSensing
                | Self::Reset
        )
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoteOff {
                channel,
                key,
                velocity,
            } => write!(f, "note off {key} vel {velocity} ch {channel}"),
            Self::NoteOn {
                channel,
                key,
                velocity,
            } => write!(f, "note on {key} vel {velocity} ch {channel}"),
            Self::PolyPressure {
                channel,
                key,
                pressure,
            } => write!(f, "poly pressure {key} {pressure} ch {channel}"),
            Self::ControlChange {
                channel,
                controller,
                value,
            } => write!(f, "CC {controller} = {value} ch {channel}"),
            Self::ProgramChange { channel, program } => {
                write!(f, "program {program} ch {channel}")
            }
            Self::ChannelPressure { channel, pressure } => {
                write!(f, "channel pressure {pressure} ch {channel}")
            }
            Self::PitchBend { channel, value } => write!(f, "pitch bend {value} ch {channel}"),
            Self::TimeCodeQuarterFrame { piece, value } => {
                write!(f, "time code piece {piece} = {value}")
            }
            Self::SongPosition { sixteenths } => write!(f, "song position {sixteenths}"),
            Self::SongSelect { song } => write!(f, "song select {song}"),
            Self::TuneRequest => f.write_str("tune request"),
            Self::TimingClock => f.write_str("clock"),
            Self::Start => f.write_str("start"),
            Self::Continue => f.write_str("continue"),
            Self::Stop => f.write_str("stop"),
            Self::ActiveSensing => f.write_str("active sensing"),
            Self::Reset => f.write_str("reset"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(n: u8) -> Channel {
        Channel::new(n).unwrap_or(Channel(0))
    }

    #[test]
    fn every_status_byte_parses_or_is_refused_and_round_trips() {
        for status in 0x80_u8..=0xFF {
            let len = message_len(status);
            let mut bytes = vec![status];
            bytes.extend((1..len).map(|i| (((i as u8) * 17) & 0x7F) | 1));
            match Message::parse(&bytes) {
                Ok(msg) => {
                    assert_eq!(msg.len(), len, "{status:#04x}");
                    // A note-on with velocity 0 is a note-off, and is written as one.
                    assert_eq!(&msg.to_bytes()[..len], &bytes[..], "{status:#04x}");
                }
                Err(e) => assert!(
                    matches!(e, ParseError::SysEx | ParseError::Undefined(_)),
                    "{status:#04x}: {e}"
                ),
            }
        }
    }

    #[test]
    fn note_on_with_velocity_zero_is_a_note_off() {
        assert_eq!(
            Message::parse(&[0x93, 60, 0]),
            Ok(Message::NoteOff {
                channel: ch(3),
                key: 60,
                velocity: 0
            })
        );
        assert_eq!(
            Message::parse(&[0x93, 60, 100]),
            Ok(Message::NoteOn {
                channel: ch(3),
                key: 60,
                velocity: 100
            })
        );
    }

    #[test]
    fn pitch_bend_and_song_position_are_fourteen_bits_little_end_first() {
        assert_eq!(
            Message::parse(&[0xE0, 0x00, 0x40]),
            Ok(Message::PitchBend {
                channel: ch(0),
                value: 0
            })
        );
        assert_eq!(
            Message::parse(&[0xE1, 0x00, 0x00]),
            Ok(Message::PitchBend {
                channel: ch(1),
                value: -8192
            })
        );
        assert_eq!(
            Message::parse(&[0xE1, 0x7F, 0x7F]),
            Ok(Message::PitchBend {
                channel: ch(1),
                value: 8191
            })
        );
        assert_eq!(
            Message::parse(&[0xF2, 0x01, 0x02]),
            Ok(Message::SongPosition { sixteenths: 257 })
        );
    }

    #[test]
    fn bad_bytes_are_refused_with_a_reason() {
        assert_eq!(Message::parse(&[]), Err(ParseError::Empty));
        assert_eq!(Message::parse(&[0x40, 1]), Err(ParseError::NoStatus(0x40)));
        assert_eq!(Message::parse(&[0xF0, 1, 0xF7]), Err(ParseError::SysEx));
        assert_eq!(Message::parse(&[0xF9]), Err(ParseError::Undefined(0xF9)));
        assert_eq!(
            Message::parse(&[0x90, 60]),
            Err(ParseError::Length {
                expected: 3,
                got: 2
            })
        );
        assert_eq!(
            Message::parse(&[0xF8, 0]),
            Err(ParseError::Length {
                expected: 1,
                got: 2
            })
        );
        assert_eq!(
            Message::parse(&[0xB0, 7, 0x80]),
            Err(ParseError::DataByte(0x80))
        );
    }

    #[test]
    fn channels_show_as_people_count_them() {
        assert_eq!(ch(0).to_string(), "1");
        assert_eq!(ch(15).number(), 16);
        assert_eq!(Channel::new(16), None);
        assert_eq!(
            Message::parse(&[0xB9, 7, 100]).map(|m| m.to_string()),
            Ok("CC 7 = 100 ch 10".to_owned())
        );
        assert_eq!(
            Message::parse(&[0xB9, 7, 100]).map(|m| m.channel()),
            Ok(Some(ch(9)))
        );
        assert!(Message::TimingClock.is_real_time());
        assert!(!Message::SongSelect { song: 1 }.is_real_time());
    }
}
