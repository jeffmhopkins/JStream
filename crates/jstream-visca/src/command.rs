//! VISCA commands and inquiries: what a controller sends a camera.
//!
//! Every message is an address byte (`8x`, `x` the camera 1 to 7, or `88` to broadcast),
//! the body, and the terminator `FF`. [`Command::encode`] and [`Inquiry::encode`] write
//! them; [`Request::parse`] reads either, as a camera (or JStream's virtual camera,
//! #1181) receives them.

use crate::{Bytes, Error};

/// A camera address on the VISCA bus: 1 to 7. [`Address::BROADCAST`] is 8.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Address(u8);

impl Address {
    /// The broadcast address, `88`.
    pub const BROADCAST: Self = Self(8);

    /// The address `n`, 1 to 7, or `None`.
    #[must_use]
    pub const fn new(n: u8) -> Option<Self> {
        if n >= 1 && n <= 7 {
            Some(Self(n))
        } else {
            None
        }
    }

    /// The address's number, 1 to 8.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }

    /// The byte a command to this address starts with: `8x`.
    #[must_use]
    pub const fn command_byte(self) -> u8 {
        0x80 | self.0
    }

    /// The byte a reply from this address starts with: `y0`, `y` being 8 plus the
    /// address (`90` from camera 1).
    #[must_use]
    pub const fn reply_byte(self) -> u8 {
        (8 + self.0) << 4
    }
}

/// Horizontal direction of a pan-tilt drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pan {
    /// `01`.
    Left,
    /// `02`.
    Right,
    /// `03`.
    Stop,
}

/// Vertical direction of a pan-tilt drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tilt {
    /// `01`.
    Up,
    /// `02`.
    Down,
    /// `03`.
    Stop,
}

/// A zoom or focus drive: stop, or move one way at the camera's standard speed or at a
/// variable speed 0 (slowest) to 7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drive {
    /// `00`.
    Stop,
    /// Zoom: tele (in). Focus: far. `02`, or `2p` with a speed.
    Plus(Option<u8>),
    /// Zoom: wide (out). Focus: near. `03`, or `3p` with a speed.
    Minus(Option<u8>),
}

/// Focus mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusMode {
    /// `02`.
    Auto,
    /// `03`.
    Manual,
    /// `10`: auto and manual in turn.
    Toggle,
}

/// What a preset command does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetOp {
    /// `00`: clear the preset.
    Reset,
    /// `01`: store the current pose in it.
    Set,
    /// `02`: move to it.
    Recall,
}

/// How many nibbles an absolute or relative pan position takes. Sony's command list
/// gives pan five nibbles and tilt four; many other heads use four for each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PositionWidth {
    /// Pan in five nibbles, tilt in four (Sony).
    #[default]
    Pan5Tilt4,
    /// Pan and tilt in four nibbles each.
    Pan4Tilt4,
}

impl PositionWidth {
    const fn pan_nibbles(self) -> usize {
        match self {
            Self::Pan5Tilt4 => 5,
            Self::Pan4Tilt4 => 4,
        }
    }
}

/// The highest pan drive speed, `18`.
pub const MAX_PAN_SPEED: u8 = 0x18;
/// The highest tilt drive speed, `17`.
pub const MAX_TILT_SPEED: u8 = 0x17;

/// A VISCA command (`8x 01 …`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// `8x 01 00 01 FF`: clears the camera's command buffers.
    IfClear,
    /// `8x 01 06 01 VV WW 0p 0t FF`: drive pan and tilt at speeds 1 to
    /// [`MAX_PAN_SPEED`] and [`MAX_TILT_SPEED`].
    PanTiltDrive {
        /// Pan speed.
        pan_speed: u8,
        /// Tilt speed.
        tilt_speed: u8,
        /// Pan direction.
        pan: Pan,
        /// Tilt direction.
        tilt: Tilt,
    },
    /// `8x 01 06 02 VV WW <pan> <tilt> FF`: move to an absolute position, in the
    /// camera's own signed counts.
    PanTiltAbsolute {
        /// Pan speed.
        pan_speed: u8,
        /// Tilt speed.
        tilt_speed: u8,
        /// Pan position.
        pan: i32,
        /// Tilt position.
        tilt: i32,
        /// How many nibbles the pan position takes.
        width: PositionWidth,
    },
    /// `8x 01 06 03 VV WW <pan> <tilt> FF`: move by a relative amount.
    PanTiltRelative {
        /// Pan speed.
        pan_speed: u8,
        /// Tilt speed.
        tilt_speed: u8,
        /// Pan change.
        pan: i32,
        /// Tilt change.
        tilt: i32,
        /// How many nibbles the pan amount takes.
        width: PositionWidth,
    },
    /// `8x 01 06 04 FF`: go to the home position.
    Home,
    /// `8x 01 06 05 FF`: re-initialise pan and tilt.
    PanTiltReset,
    /// `8x 01 04 07 xx FF`: drive the zoom.
    Zoom(Drive),
    /// `8x 01 04 47 0p 0q 0r 0s FF`: zoom to a position, 0 the widest.
    ZoomDirect(u16),
    /// `8x 01 04 08 xx FF`: drive the focus.
    Focus(Drive),
    /// `8x 01 04 48 0p 0q 0r 0s FF`: focus at a position.
    FocusDirect(u16),
    /// `8x 01 04 38 xx FF`: set the focus mode.
    FocusMode(FocusMode),
    /// `8x 01 04 18 01 FF`: focus once, automatically.
    FocusOnePush,
    /// `8x 01 04 3F 0o pp FF`: reset, set or recall preset `pp` (0 to 127).
    Preset(PresetOp, u8),
    /// `8x 01 04 00 02/03 FF`: power on or standby.
    Power(bool),
}

/// A VISCA inquiry (`8x 09 …`): a question whose reply carries data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inquiry {
    /// `8x 09 04 00 FF`: power.
    Power,
    /// `8x 09 04 47 FF`: zoom position.
    ZoomPosition,
    /// `8x 09 04 48 FF`: focus position.
    FocusPosition,
    /// `8x 09 04 38 FF`: focus mode.
    FocusMode,
    /// `8x 09 06 12 FF`: pan and tilt position.
    PanTiltPosition,
}

/// A message to a camera, from [`Request::parse`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// A command.
    Command(Command),
    /// An inquiry.
    Inquiry(Inquiry),
}

fn drive_byte(d: Drive) -> Result<u8, Error> {
    Ok(match d {
        Drive::Stop => 0x00,
        Drive::Plus(None) => 0x02,
        Drive::Minus(None) => 0x03,
        Drive::Plus(Some(p)) if p <= 7 => 0x20 | p,
        Drive::Minus(Some(p)) if p <= 7 => 0x30 | p,
        _ => return Err(Error::OutOfRange("drive speed")),
    })
}

fn parse_drive(b: u8) -> Option<Drive> {
    Some(match b {
        0x00 => Drive::Stop,
        0x02 => Drive::Plus(None),
        0x03 => Drive::Minus(None),
        0x20..=0x27 => Drive::Plus(Some(b & 7)),
        0x30..=0x37 => Drive::Minus(Some(b & 7)),
        _ => return None,
    })
}

/// Writes `value` as `n` nibbles, one per byte, most significant first, two's complement.
pub(crate) fn put_nibbles(out: &mut Bytes, value: i64, n: usize) {
    for i in (0..n).rev() {
        out.push(((value >> (4 * i)) & 0x0F) as u8);
    }
}

/// Reads `n` nibble bytes as an unsigned value, or `None` if a byte isn't `0x`.
pub(crate) fn get_nibbles(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |acc, &b| {
        (b <= 0x0F).then(|| acc << 4 | u32::from(b))
    })
}

/// A nibble value as signed, two's complement in `n` nibbles.
pub(crate) fn signed(value: u32, n: usize) -> i32 {
    let bits = 4 * n as u32;
    let sign = 1_u32 << (bits - 1);
    if value & sign != 0 {
        (i64::from(value) - (1_i64 << bits)) as i32
    } else {
        value as i32
    }
}

/// Whether `v` fits `n` nibbles as two's complement.
pub(crate) fn fits(v: i32, n: usize) -> bool {
    let half = 1_i64 << (4 * n - 1);
    (-half..half).contains(&i64::from(v))
}

fn check_speeds(pan_speed: u8, tilt_speed: u8) -> Result<(), Error> {
    if !(1..=MAX_PAN_SPEED).contains(&pan_speed) {
        return Err(Error::OutOfRange("pan speed"));
    }
    if !(1..=MAX_TILT_SPEED).contains(&tilt_speed) {
        return Err(Error::OutOfRange("tilt speed"));
    }
    Ok(())
}

impl Command {
    /// The command's bytes, addressed to `to`.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfRange`] for a speed, position or preset out of the range the
    /// command can carry.
    pub fn encode(&self, to: Address) -> Result<Bytes, Error> {
        let mut b = Bytes::new();
        b.push(to.command_byte());
        b.push(0x01);
        match *self {
            Self::IfClear => b.extend(&[0x00, 0x01]),
            Self::PanTiltDrive {
                pan_speed,
                tilt_speed,
                pan,
                tilt,
            } => {
                check_speeds(pan_speed, tilt_speed)?;
                let p = match pan {
                    Pan::Left => 1,
                    Pan::Right => 2,
                    Pan::Stop => 3,
                };
                let t = match tilt {
                    Tilt::Up => 1,
                    Tilt::Down => 2,
                    Tilt::Stop => 3,
                };
                b.extend(&[0x06, 0x01, pan_speed, tilt_speed, p, t]);
            }
            Self::PanTiltAbsolute {
                pan_speed,
                tilt_speed,
                pan,
                tilt,
                width,
            }
            | Self::PanTiltRelative {
                pan_speed,
                tilt_speed,
                pan,
                tilt,
                width,
            } => {
                check_speeds(pan_speed, tilt_speed)?;
                let n = width.pan_nibbles();
                if !fits(pan, n) {
                    return Err(Error::OutOfRange("pan position"));
                }
                if !fits(tilt, 4) {
                    return Err(Error::OutOfRange("tilt position"));
                }
                let kind = if matches!(self, Self::PanTiltAbsolute { .. }) {
                    0x02
                } else {
                    0x03
                };
                b.extend(&[0x06, kind, pan_speed, tilt_speed]);
                put_nibbles(&mut b, i64::from(pan), n);
                put_nibbles(&mut b, i64::from(tilt), 4);
            }
            Self::Home => b.extend(&[0x06, 0x04]),
            Self::PanTiltReset => b.extend(&[0x06, 0x05]),
            Self::Zoom(d) => b.extend(&[0x04, 0x07, drive_byte(d)?]),
            Self::ZoomDirect(v) => {
                b.extend(&[0x04, 0x47]);
                put_nibbles(&mut b, i64::from(v), 4);
            }
            Self::Focus(d) => b.extend(&[0x04, 0x08, drive_byte(d)?]),
            Self::FocusDirect(v) => {
                b.extend(&[0x04, 0x48]);
                put_nibbles(&mut b, i64::from(v), 4);
            }
            Self::FocusMode(m) => b.extend(&[
                0x04,
                0x38,
                match m {
                    FocusMode::Auto => 0x02,
                    FocusMode::Manual => 0x03,
                    FocusMode::Toggle => 0x10,
                },
            ]),
            Self::FocusOnePush => b.extend(&[0x04, 0x18, 0x01]),
            Self::Preset(op, n) => {
                if n > 0x7F {
                    return Err(Error::OutOfRange("preset"));
                }
                let o = match op {
                    PresetOp::Reset => 0,
                    PresetOp::Set => 1,
                    PresetOp::Recall => 2,
                };
                b.extend(&[0x04, 0x3F, o, n]);
            }
            Self::Power(on) => b.extend(&[0x04, 0x00, if on { 0x02 } else { 0x03 }]),
        }
        b.push(0xFF);
        Ok(b)
    }
}

impl Inquiry {
    /// The inquiry's bytes, addressed to `to`.
    #[must_use]
    pub fn encode(&self, to: Address) -> Bytes {
        let mut b = Bytes::new();
        b.push(to.command_byte());
        b.extend(match self {
            Self::Power => &[0x09, 0x04, 0x00],
            Self::ZoomPosition => &[0x09, 0x04, 0x47],
            Self::FocusPosition => &[0x09, 0x04, 0x48],
            Self::FocusMode => &[0x09, 0x04, 0x38],
            Self::PanTiltPosition => &[0x09, 0x06, 0x12],
        });
        b.push(0xFF);
        b
    }
}

impl Request {
    /// Parses one message to a camera: its address and what it asks.
    ///
    /// # Errors
    ///
    /// [`Error::Syntax`] for bytes that aren't a VISCA message (no `8x` address, no
    /// final `FF`, a stray `FF` or a byte out of range), and [`Error::Unsupported`] for
    /// a well-formed message this crate doesn't know, which a camera answers "not
    /// executable".
    pub fn parse(bytes: &[u8]) -> Result<(Address, Self), Error> {
        let (&first, rest) = bytes.split_first().ok_or(Error::Syntax)?;
        let (&last, body) = rest.split_last().ok_or(Error::Syntax)?;
        if first & 0xF0 != 0x80 || !(1..=8).contains(&(first & 0x0F)) || last != 0xFF {
            return Err(Error::Syntax);
        }
        if body.contains(&0xFF) {
            return Err(Error::Syntax);
        }
        let to = Address(first & 0x0F);
        let speeds = |v: u8, w: u8| check_speeds(v, w).map_err(|_| Error::Syntax);
        let req = match body {
            [0x01, 0x00, 0x01] => Self::Command(Command::IfClear),
            [0x01, 0x06, 0x01, v, w, p, t] => {
                speeds(*v, *w)?;
                let pan = match p {
                    1 => Pan::Left,
                    2 => Pan::Right,
                    3 => Pan::Stop,
                    _ => return Err(Error::Syntax),
                };
                let tilt = match t {
                    1 => Tilt::Up,
                    2 => Tilt::Down,
                    3 => Tilt::Stop,
                    _ => return Err(Error::Syntax),
                };
                Self::Command(Command::PanTiltDrive {
                    pan_speed: *v,
                    tilt_speed: *w,
                    pan,
                    tilt,
                })
            }
            [0x01, 0x06, kind @ (0x02 | 0x03), v, w, pos @ ..] => {
                speeds(*v, *w)?;
                let width = match pos.len() {
                    9 => PositionWidth::Pan5Tilt4,
                    8 => PositionWidth::Pan4Tilt4,
                    _ => return Err(Error::Syntax),
                };
                let n = width.pan_nibbles();
                let pan = signed(get_nibbles(&pos[..n]).ok_or(Error::Syntax)?, n);
                let tilt = signed(get_nibbles(&pos[n..]).ok_or(Error::Syntax)?, 4);
                Self::Command(if *kind == 0x02 {
                    Command::PanTiltAbsolute {
                        pan_speed: *v,
                        tilt_speed: *w,
                        pan,
                        tilt,
                        width,
                    }
                } else {
                    Command::PanTiltRelative {
                        pan_speed: *v,
                        tilt_speed: *w,
                        pan,
                        tilt,
                        width,
                    }
                })
            }
            [0x01, 0x06, 0x04] => Self::Command(Command::Home),
            [0x01, 0x06, 0x05] => Self::Command(Command::PanTiltReset),
            [0x01, 0x04, 0x07, d] => {
                Self::Command(Command::Zoom(parse_drive(*d).ok_or(Error::Syntax)?))
            }
            [0x01, 0x04, 0x47, p @ ..] if p.len() == 4 => Self::Command(Command::ZoomDirect(
                get_nibbles(p).ok_or(Error::Syntax)? as u16,
            )),
            [0x01, 0x04, 0x08, d] => {
                Self::Command(Command::Focus(parse_drive(*d).ok_or(Error::Syntax)?))
            }
            [0x01, 0x04, 0x48, p @ ..] if p.len() == 4 => Self::Command(Command::FocusDirect(
                get_nibbles(p).ok_or(Error::Syntax)? as u16,
            )),
            [0x01, 0x04, 0x38, 0x02] => Self::Command(Command::FocusMode(FocusMode::Auto)),
            [0x01, 0x04, 0x38, 0x03] => Self::Command(Command::FocusMode(FocusMode::Manual)),
            [0x01, 0x04, 0x38, 0x10] => Self::Command(Command::FocusMode(FocusMode::Toggle)),
            [0x01, 0x04, 0x18, 0x01] => Self::Command(Command::FocusOnePush),
            [0x01, 0x04, 0x3F, o @ 0..=2, n] if *n <= 0x7F => {
                let op = match o {
                    0 => PresetOp::Reset,
                    1 => PresetOp::Set,
                    _ => PresetOp::Recall,
                };
                Self::Command(Command::Preset(op, *n))
            }
            [0x01, 0x04, 0x00, 0x02] => Self::Command(Command::Power(true)),
            [0x01, 0x04, 0x00, 0x03] => Self::Command(Command::Power(false)),
            [0x09, 0x04, 0x00] => Self::Inquiry(Inquiry::Power),
            [0x09, 0x04, 0x47] => Self::Inquiry(Inquiry::ZoomPosition),
            [0x09, 0x04, 0x48] => Self::Inquiry(Inquiry::FocusPosition),
            [0x09, 0x04, 0x38] => Self::Inquiry(Inquiry::FocusMode),
            [0x09, 0x06, 0x12] => Self::Inquiry(Inquiry::PanTiltPosition),
            [0x01 | 0x09, ..] => return Err(Error::Unsupported),
            _ => return Err(Error::Syntax),
        };
        Ok((to, req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam1() -> Address {
        Address::new(1).unwrap_or(Address::BROADCAST)
    }

    #[test]
    fn commands_encode_to_the_documented_bytes() {
        let cases: [(Command, &[u8]); 14] = [
            (
                Command::PanTiltDrive {
                    pan_speed: 0x18,
                    tilt_speed: 0x17,
                    pan: Pan::Left,
                    tilt: Tilt::Up,
                },
                &[0x81, 0x01, 0x06, 0x01, 0x18, 0x17, 0x01, 0x01, 0xFF],
            ),
            (Command::Home, &[0x81, 0x01, 0x06, 0x04, 0xFF]),
            (Command::PanTiltReset, &[0x81, 0x01, 0x06, 0x05, 0xFF]),
            (
                Command::Zoom(Drive::Stop),
                &[0x81, 0x01, 0x04, 0x07, 0x00, 0xFF],
            ),
            (
                Command::Zoom(Drive::Plus(None)),
                &[0x81, 0x01, 0x04, 0x07, 0x02, 0xFF],
            ),
            (
                Command::Zoom(Drive::Minus(Some(5))),
                &[0x81, 0x01, 0x04, 0x07, 0x35, 0xFF],
            ),
            (
                Command::ZoomDirect(0x4000),
                &[0x81, 0x01, 0x04, 0x47, 0x04, 0x00, 0x00, 0x00, 0xFF],
            ),
            (
                Command::Focus(Drive::Plus(Some(2))),
                &[0x81, 0x01, 0x04, 0x08, 0x22, 0xFF],
            ),
            (
                Command::FocusMode(FocusMode::Auto),
                &[0x81, 0x01, 0x04, 0x38, 0x02, 0xFF],
            ),
            (Command::FocusOnePush, &[0x81, 0x01, 0x04, 0x18, 0x01, 0xFF]),
            (
                Command::Preset(PresetOp::Recall, 3),
                &[0x81, 0x01, 0x04, 0x3F, 0x02, 0x03, 0xFF],
            ),
            (
                Command::Preset(PresetOp::Set, 9),
                &[0x81, 0x01, 0x04, 0x3F, 0x01, 0x09, 0xFF],
            ),
            (Command::Power(false), &[0x81, 0x01, 0x04, 0x00, 0x03, 0xFF]),
            (Command::IfClear, &[0x81, 0x01, 0x00, 0x01, 0xFF]),
        ];
        for (cmd, bytes) in cases {
            let got = cmd.encode(cam1()).unwrap();
            assert_eq!(got.as_slice(), bytes, "{cmd:?}");
            assert_eq!(Request::parse(bytes), Ok((cam1(), Request::Command(cmd))));
        }
    }

    #[test]
    fn positions_are_signed_nibbles_in_either_width() {
        let abs = Command::PanTiltAbsolute {
            pan_speed: 0x10,
            tilt_speed: 0x10,
            pan: -1,
            tilt: 0x123,
            width: PositionWidth::Pan5Tilt4,
        };
        let b = abs.encode(cam1()).unwrap();
        assert_eq!(
            b.as_slice(),
            &[
                0x81, 0x01, 0x06, 0x02, 0x10, 0x10, 0x0F, 0x0F, 0x0F, 0x0F, 0x0F, 0x00, 0x01, 0x02,
                0x03, 0xFF
            ]
        );
        assert_eq!(
            Request::parse(b.as_slice()),
            Ok((cam1(), Request::Command(abs)))
        );
        let rel = Command::PanTiltRelative {
            pan_speed: 1,
            tilt_speed: 1,
            pan: -0x8000,
            tilt: 0x7FFF,
            width: PositionWidth::Pan4Tilt4,
        };
        let b = rel.encode(cam1()).unwrap();
        assert_eq!(b.len(), 15);
        assert_eq!(
            Request::parse(b.as_slice()),
            Ok((cam1(), Request::Command(rel)))
        );
        let too_far = Command::PanTiltRelative {
            pan_speed: 1,
            tilt_speed: 1,
            pan: 0x8000,
            tilt: 0,
            width: PositionWidth::Pan4Tilt4,
        };
        assert_eq!(
            too_far.encode(cam1()),
            Err(Error::OutOfRange("pan position"))
        );
    }

    #[test]
    fn inquiries_encode_and_parse() {
        for (q, body) in [
            (Inquiry::Power, [0x09, 0x04, 0x00]),
            (Inquiry::ZoomPosition, [0x09, 0x04, 0x47]),
            (Inquiry::FocusPosition, [0x09, 0x04, 0x48]),
            (Inquiry::FocusMode, [0x09, 0x04, 0x38]),
            (Inquiry::PanTiltPosition, [0x09, 0x06, 0x12]),
        ] {
            let b = q.encode(cam1());
            assert_eq!(&b.as_slice()[1..4], &body);
            assert_eq!(
                Request::parse(b.as_slice()),
                Ok((cam1(), Request::Inquiry(q)))
            );
        }
    }

    #[test]
    fn bad_and_unknown_messages_are_told_apart() {
        assert_eq!(Request::parse(&[]), Err(Error::Syntax));
        assert_eq!(
            Request::parse(&[0x81, 0x01, 0x06, 0x04]),
            Err(Error::Syntax)
        );
        assert_eq!(
            Request::parse(&[0x41, 0x01, 0x06, 0x04, 0xFF]),
            Err(Error::Syntax)
        );
        assert_eq!(
            Request::parse(&[0x81, 0x01, 0xFF, 0x04, 0xFF]),
            Err(Error::Syntax)
        );
        // Speed 0 isn't a speed.
        assert_eq!(
            Request::parse(&[0x81, 0x01, 0x06, 0x01, 0x00, 0x01, 0x03, 0x03, 0xFF]),
            Err(Error::Syntax)
        );
        // Exposure: well formed, not something a virtual camera does.
        assert_eq!(
            Request::parse(&[0x81, 0x01, 0x04, 0x39, 0x00, 0xFF]),
            Err(Error::Unsupported)
        );
        assert_eq!(
            Request::parse(&[0x88, 0x01, 0x06, 0x04, 0xFF]),
            Ok((Address::BROADCAST, Request::Command(Command::Home)))
        );
        assert_eq!(
            Command::PanTiltDrive {
                pan_speed: 0x19,
                tilt_speed: 1,
                pan: Pan::Stop,
                tilt: Tilt::Stop
            }
            .encode(cam1()),
            Err(Error::OutOfRange("pan speed"))
        );
        assert_eq!(
            Command::Preset(PresetOp::Recall, 0x80).encode(cam1()),
            Err(Error::OutOfRange("preset"))
        );
    }
}
