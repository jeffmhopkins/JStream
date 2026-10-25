//! The control channel between the engine and a plugin host: framed messages over the
//! host's standard input and output. The audio and the parameter changes go through the
//! shared block; this channel carries what is rare and variable-sized: the plugin's saved
//! state, its parameter list, and the scan of a plugin file.
//!
//! **Framing.** Each message is a little-endian `u32` payload length, then the payload:
//! a tag byte and the message's fields. A string or a byte string is a `u32` length and
//! its bytes; numbers are little-endian. A frame over [`MAX_FRAME`] bytes, an unknown tag,
//! a field running past the frame, or bytes left over after the last field is
//! [`DecodeError`]: each side treats the other's frames as untrusted input.

use crate::ParamEvent;

/// The largest frame payload, bytes: a 2 MiB saved state with room to spare.
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

/// The most entries a list (plugins, parameters) may hold in one message.
pub const MAX_LIST: usize = 16_384;

/// The largest saved state, bytes, the engine takes from a host (ROADMAP M3: a CLAP
/// filter's state may be 2 MiB). The host refuses to save more, and the engine ignores a
/// larger [`FromHost::State`].
pub const MAX_STATE: usize = 2 * 1024 * 1024;

/// One of a plugin's parameters, as the host reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamDesc {
    /// The plugin's id for it, which [`ParamEvent`]s carry.
    pub id: u32,
    /// Its name.
    pub name: String,
    /// The group it belongs to (`"Filter/Envelope"`), or empty.
    pub module: String,
    /// Its lowest value.
    pub min: f64,
    /// Its highest value.
    pub max: f64,
    /// Its default value.
    pub default: f64,
    /// `PARAM_*` flags.
    pub flags: u32,
}

impl ParamDesc {
    /// Whether the range is usable: `min`, `max` and `default` are finite, `min` is at
    /// most `max`, and `default` is within them. A host is another process, so the
    /// engine keeps only descriptions that pass ([`valid_params`]).
    pub fn is_valid(&self) -> bool {
        self.min.is_finite()
            && self.max.is_finite()
            && self.default.is_finite()
            && self.min <= self.max
            && (self.min..=self.max).contains(&self.default)
    }

    /// Whether `value` may be this parameter's value: finite and within its range.
    pub fn accepts(&self, value: f64) -> bool {
        value.is_finite() && (self.min..=self.max).contains(&value)
    }
}

/// The parameter descriptions a host sent, minus the ones the engine can't use: those
/// that aren't [`ParamDesc::is_valid`], and any whose id an earlier one already has.
/// Also returns how many were dropped, for the log.
pub fn valid_params(params: Vec<ParamDesc>) -> (Vec<ParamDesc>, usize) {
    let sent = params.len();
    let mut kept: Vec<ParamDesc> = Vec::with_capacity(sent);
    for p in params {
        if p.is_valid() && !kept.iter().any(|k| k.id == p.id) {
            kept.push(p);
        }
    }
    let dropped = sent - kept.len();
    (kept, dropped)
}

/// `event` when it names one of `params` and [`ParamDesc::accepts`] its value: the check
/// a parameter change coming back from a host (its control messages or the block's
/// `params_out` ring) passes before the engine uses it.
pub fn check_param_event(params: &[ParamDesc], event: ParamEvent) -> Option<ParamEvent> {
    params
        .iter()
        .find(|p| p.id == event.id)
        .filter(|p| p.accepts(event.value))
        .map(|_| event)
}

/// Whole steps only.
pub const PARAM_STEPPED: u32 = 1;
/// One of a set of named values.
pub const PARAM_ENUM: u32 = 1 << 1;
/// Not shown to the user.
pub const PARAM_HIDDEN: u32 = 1 << 2;
/// Reported by the plugin, never set.
pub const PARAM_READ_ONLY: u32 = 1 << 3;
/// The plugin's own bypass switch.
pub const PARAM_BYPASS: u32 = 1 << 4;

/// One audio effect in a plugin file, as a scan reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginDesc {
    /// The plugin's CLAP id (`com.vendor.effect`).
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its vendor, or empty.
    pub vendor: String,
    /// Its version, or empty.
    pub version: String,
    /// Its description, or empty.
    pub description: String,
    /// Its parameters, in its order.
    pub params: Vec<ParamDesc>,
}

/// A message from the engine to a host.
#[derive(Clone, Debug, PartialEq)]
pub enum ToHost {
    /// Where the shared block is, sent before `Start` where the platform hands the block
    /// over as a handle (Windows): `handle` is the block's unnamed section, already
    /// duplicated into the host's process, so no other process can open it by name.
    Block {
        /// The section handle's value in the host's process.
        handle: u64,
    },
    /// The first message of a run (after any `Block`): start the plugin, restoring
    /// `state` first when it is given.
    Start {
        /// A state an earlier run saved, or `None` for the plugin's defaults.
        state: Option<Vec<u8>>,
    },
    /// Save the plugin's state and answer with [`FromHost::State`].
    SaveState,
    /// Deactivate the plugin and exit. The engine also sets the block's `stop` word.
    Stop,
}

/// A message from a host to the engine.
#[derive(Clone, Debug, PartialEq)]
pub enum FromHost {
    /// The plugin's parameters, sent once it has started and again whenever the plugin
    /// says they changed.
    Params(Vec<ParamDesc>),
    /// The plugin's saved state, answering [`ToHost::SaveState`]; empty when it has
    /// none.
    State(Vec<u8>),
    /// The plugin marked its state changed: worth saving.
    StateDirty,
    /// The plugin changed a parameter itself (its own editor). Changes made while
    /// processing come back through the shared block instead.
    ParamChanged(ParamEvent),
    /// A scan's answer: the file's audio effects. The host exits after it.
    Plugins(Vec<PluginDesc>),
    /// Something went wrong, in words, for the engine's log.
    Error(String),
}

/// Why a frame couldn't be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The length prefix is over [`MAX_FRAME`].
    TooLarge,
    /// A field runs past the end of the frame, or a list is over [`MAX_LIST`].
    Truncated,
    /// The tag names no message.
    UnknownTag(u8),
    /// A string isn't UTF-8.
    NotUtf8,
    /// Bytes are left after the message's last field.
    Trailing,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "frame over {MAX_FRAME} bytes"),
            Self::Truncated => write!(f, "frame ends inside a field"),
            Self::UnknownTag(tag) => write!(f, "unknown message tag {tag}"),
            Self::NotUtf8 => write!(f, "string is not UTF-8"),
            Self::Trailing => write!(f, "bytes after the message"),
        }
    }
}

impl std::error::Error for DecodeError {}

const TAG_START: u8 = 1;
const TAG_SAVE_STATE: u8 = 2;
const TAG_STOP: u8 = 3;
const TAG_BLOCK: u8 = 4;
const TAG_PARAMS: u8 = 64;
const TAG_STATE: u8 = 65;
const TAG_STATE_DIRTY: u8 = 66;
const TAG_PARAM_CHANGED: u8 = 67;
const TAG_PLUGINS: u8 = 68;
const TAG_ERROR: u8 = 69;

impl ToHost {
    /// The message as one frame: its length prefix and payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::frame();
        match self {
            Self::Start { state } => {
                w.u8(TAG_START);
                w.u8(u8::from(state.is_some()));
                if let Some(state) = state {
                    w.bytes(state);
                }
            }
            Self::SaveState => w.u8(TAG_SAVE_STATE),
            Self::Stop => w.u8(TAG_STOP),
            Self::Block { handle } => {
                w.u8(TAG_BLOCK);
                w.u64(*handle);
            }
        }
        w.finish()
    }

    /// Reads one frame's payload (without its length prefix).
    ///
    /// # Errors
    /// A malformed payload.
    pub fn decode(payload: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(payload)?;
        let message = match r.u8()? {
            TAG_START => {
                let state = if r.u8()? != 0 {
                    Some(r.bytes()?.to_vec())
                } else {
                    None
                };
                Self::Start { state }
            }
            TAG_SAVE_STATE => Self::SaveState,
            TAG_STOP => Self::Stop,
            TAG_BLOCK => Self::Block { handle: r.u64()? },
            tag => return Err(DecodeError::UnknownTag(tag)),
        };
        r.end()?;
        Ok(message)
    }
}

impl FromHost {
    /// The message as one frame: its length prefix and payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::frame();
        match self {
            Self::Params(params) => {
                w.u8(TAG_PARAMS);
                w.params(params);
            }
            Self::State(state) => {
                w.u8(TAG_STATE);
                w.bytes(state);
            }
            Self::StateDirty => w.u8(TAG_STATE_DIRTY),
            Self::ParamChanged(event) => {
                w.u8(TAG_PARAM_CHANGED);
                w.u32(event.id);
                w.f64(event.value);
            }
            Self::Plugins(plugins) => {
                w.u8(TAG_PLUGINS);
                w.len(plugins.len());
                for p in plugins {
                    for s in [&p.id, &p.name, &p.vendor, &p.version, &p.description] {
                        w.str(s);
                    }
                    w.params(&p.params);
                }
            }
            Self::Error(text) => {
                w.u8(TAG_ERROR);
                w.str(text);
            }
        }
        w.finish()
    }

    /// Reads one frame's payload (without its length prefix).
    ///
    /// # Errors
    /// A malformed payload.
    pub fn decode(payload: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(payload)?;
        let message = match r.u8()? {
            TAG_PARAMS => Self::Params(r.params()?),
            TAG_STATE => Self::State(r.bytes()?.to_vec()),
            TAG_STATE_DIRTY => Self::StateDirty,
            TAG_PARAM_CHANGED => Self::ParamChanged(ParamEvent {
                id: r.u32()?,
                value: r.f64()?,
            }),
            TAG_PLUGINS => {
                let n = r.len()?;
                let mut plugins = Vec::new();
                for _ in 0..n {
                    plugins.push(PluginDesc {
                        id: r.str()?,
                        name: r.str()?,
                        vendor: r.str()?,
                        version: r.str()?,
                        description: r.str()?,
                        params: r.params()?,
                    });
                }
                Self::Plugins(plugins)
            }
            TAG_ERROR => Self::Error(r.str()?),
            tag => return Err(DecodeError::UnknownTag(tag)),
        };
        r.end()?;
        Ok(message)
    }
}

/// Reads one frame from `input`: `Ok(None)` at a clean end of the stream.
///
/// # Errors
/// An I/O error, a stream ending inside a frame, or a length over [`MAX_FRAME`]
/// (`InvalidData`).
pub fn read_frame(input: &mut impl std::io::Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < len.len() {
        match input.read(&mut len[got..])? {
            0 if got == 0 => return Ok(None),
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => got += n,
        }
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            DecodeError::TooLarge,
        ));
    }
    let mut payload = vec![0; len];
    input.read_exact(&mut payload)?;
    Ok(Some(payload))
}

struct Writer(Vec<u8>);

impl Writer {
    /// A frame with room for its length prefix.
    fn frame() -> Self {
        Self(vec![0; 4])
    }

    fn finish(mut self) -> Vec<u8> {
        let len = (self.0.len() - 4) as u32;
        self.0[..4].copy_from_slice(&len.to_le_bytes());
        self.0
    }

    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }

    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn len(&mut self, n: usize) {
        self.u32(u32::try_from(n).unwrap_or(u32::MAX));
    }

    fn bytes(&mut self, b: &[u8]) {
        self.len(b.len());
        self.0.extend_from_slice(b);
    }

    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }

    fn params(&mut self, params: &[ParamDesc]) {
        self.len(params.len());
        for p in params {
            self.u32(p.id);
            self.str(&p.name);
            self.str(&p.module);
            self.f64(p.min);
            self.f64(p.max);
            self.f64(p.default);
            self.u32(p.flags);
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn new(payload: &'a [u8]) -> Result<Self, DecodeError> {
        if payload.len() > MAX_FRAME {
            return Err(DecodeError::TooLarge);
        }
        Ok(Self(payload))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if n > self.0.len() {
            return Err(DecodeError::Truncated);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut a = [0; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        self.array().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        self.array().map(u64::from_le_bytes)
    }

    fn f64(&mut self) -> Result<f64, DecodeError> {
        self.array().map(f64::from_le_bytes)
    }

    /// A list length, at most [`MAX_LIST`].
    fn len(&mut self) -> Result<usize, DecodeError> {
        let n = self.u32()? as usize;
        if n > MAX_LIST {
            return Err(DecodeError::Truncated);
        }
        Ok(n)
    }

    fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    fn str(&mut self) -> Result<String, DecodeError> {
        std::str::from_utf8(self.bytes()?)
            .map(str::to_owned)
            .map_err(|_| DecodeError::NotUtf8)
    }

    fn params(&mut self) -> Result<Vec<ParamDesc>, DecodeError> {
        let n = self.len()?;
        let mut params = Vec::new();
        for _ in 0..n {
            params.push(ParamDesc {
                id: self.u32()?,
                name: self.str()?,
                module: self.str()?,
                min: self.f64()?,
                max: self.f64()?,
                default: self.f64()?,
                flags: self.u32()?,
            });
        }
        Ok(params)
    }

    fn end(&self) -> Result<(), DecodeError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(DecodeError::Trailing)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain() -> ParamDesc {
        ParamDesc {
            id: 7,
            name: "Gain".into(),
            module: "Main".into(),
            min: 0.0,
            max: 2.0,
            default: 1.0,
            flags: PARAM_STEPPED | PARAM_BYPASS,
        }
    }

    /// Encodes, reads the frame back from a stream and decodes it.
    fn round_trip<T>(frame: &[u8], decode: fn(&[u8]) -> Result<T, DecodeError>) -> T {
        let mut stream = frame;
        let payload = read_frame(&mut stream).unwrap().unwrap();
        assert!(stream.is_empty());
        decode(&payload).unwrap()
    }

    #[test]
    fn messages_to_the_host_round_trip() {
        for m in [
            ToHost::Start { state: None },
            ToHost::Start {
                state: Some(vec![1, 2, 3]),
            },
            ToHost::SaveState,
            ToHost::Stop,
            ToHost::Block {
                handle: 0x1234_5678_9abc,
            },
        ] {
            assert_eq!(round_trip(&m.encode(), ToHost::decode), m);
        }
    }

    #[test]
    fn messages_from_the_host_round_trip() {
        for m in [
            FromHost::Params(vec![gain(), gain()]),
            FromHost::State(vec![9; 1000]),
            FromHost::StateDirty,
            FromHost::ParamChanged(ParamEvent { id: 3, value: 0.25 }),
            FromHost::Plugins(vec![PluginDesc {
                id: "org.example.eq".into(),
                name: "EQ — ünïcode".into(),
                vendor: String::new(),
                version: "1.0".into(),
                description: "An equaliser".into(),
                params: vec![gain()],
            }]),
            FromHost::Error("no such plugin".into()),
        ] {
            assert_eq!(round_trip(&m.encode(), FromHost::decode), m);
        }
    }

    #[test]
    fn malformed_payloads_are_refused() {
        assert_eq!(ToHost::decode(&[]), Err(DecodeError::Truncated));
        assert_eq!(ToHost::decode(&[200]), Err(DecodeError::UnknownTag(200)));
        assert_eq!(ToHost::decode(&[TAG_STOP, 0]), Err(DecodeError::Trailing));
        assert_eq!(
            ToHost::decode(&[TAG_BLOCK, 1, 2, 3]),
            Err(DecodeError::Truncated)
        );
        // A state claiming more bytes than the frame holds.
        assert_eq!(
            ToHost::decode(&[TAG_START, 1, 9, 0, 0, 0, 1]),
            Err(DecodeError::Truncated)
        );
        // A list over the cap.
        let mut huge = vec![TAG_PARAMS];
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(FromHost::decode(&huge), Err(DecodeError::Truncated));
        let mut bad = vec![TAG_ERROR, 2, 0, 0, 0];
        bad.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(FromHost::decode(&bad), Err(DecodeError::NotUtf8));
        // Every truncation of a valid message is refused, never a panic.
        let full = FromHost::Plugins(vec![PluginDesc {
            id: "a".into(),
            name: "b".into(),
            vendor: "c".into(),
            version: "d".into(),
            description: "e".into(),
            params: vec![gain()],
        }])
        .encode();
        let payload = &full[4..];
        for cut in 0..payload.len() {
            assert!(FromHost::decode(&payload[..cut]).is_err());
        }
    }

    #[test]
    fn streams_end_cleanly_or_not() {
        let mut empty: &[u8] = &[];
        assert!(read_frame(&mut empty).unwrap().is_none());
        let mut cut: &[u8] = &[5, 0, 0, 0, 1];
        assert!(read_frame(&mut cut).is_err());
        let mut half_len: &[u8] = &[5, 0];
        assert!(read_frame(&mut half_len).is_err());
        let too_big = ((MAX_FRAME + 1) as u32).to_le_bytes();
        let mut big: &[u8] = &too_big;
        assert_eq!(
            read_frame(&mut big).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn param_descriptions_with_unusable_ranges_are_dropped() {
        let ok = gain();
        assert!(ok.is_valid());
        let bad = [
            ParamDesc {
                min: f64::NAN,
                ..gain()
            },
            ParamDesc {
                max: f64::INFINITY,
                ..gain()
            },
            ParamDesc {
                default: f64::NEG_INFINITY,
                ..gain()
            },
            ParamDesc {
                min: 3.0,
                max: 2.0,
                default: 2.5,
                ..gain()
            },
            ParamDesc {
                default: 2.5,
                ..gain()
            },
            ParamDesc {
                default: -0.5,
                ..gain()
            },
        ];
        for p in &bad {
            assert!(!p.is_valid(), "{p:?}");
        }
        let mut sent: Vec<ParamDesc> = bad
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, p)| ParamDesc {
                id: i as u32 + 100,
                ..p
            })
            .collect();
        sent.push(ok.clone());
        // A second description of the same id is dropped too: the first one stands.
        sent.push(ParamDesc { max: 9.0, ..gain() });
        let (kept, dropped) = valid_params(sent);
        assert_eq!(kept, vec![ok]);
        assert_eq!(dropped, bad.len() + 1);
        assert_eq!(valid_params(Vec::new()), (Vec::new(), 0));
    }

    #[test]
    fn param_events_must_name_a_parameter_and_fit_its_range() {
        let params = [gain()];
        let event = |id, value| ParamEvent { id, value };
        for good in [event(7, 0.0), event(7, 1.5), event(7, 2.0)] {
            assert_eq!(check_param_event(&params, good), Some(good));
        }
        for bad in [
            event(8, 1.0),
            event(7, f64::NAN),
            event(7, f64::INFINITY),
            event(7, f64::NEG_INFINITY),
            event(7, -0.001),
            event(7, 2.001),
            event(7, f64::MAX),
        ] {
            assert_eq!(check_param_event(&params, bad), None, "{bad:?}");
        }
        assert_eq!(check_param_event(&[], event(7, 1.0)), None);
    }
}
