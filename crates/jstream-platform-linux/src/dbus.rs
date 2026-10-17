//! A minimal D-Bus client for the ScreenCast portal (ADR-0060 §3).
//!
//! The portal needs a handful of method calls and two signals, so this is a small
//! implementation of the [D-Bus wire protocol](https://dbus.freedesktop.org/doc/dbus-specification.html)
//! rather than a dependency: values and their signatures, message marshalling in both
//! byte orders, and the `EXTERNAL` authentication lines. It is platform-independent and
//! tested everywhere. The socket transport, which passes Unix file descriptors, is
//! [`bus`] and Linux-only.
//!
//! Received messages are untrusted input: every length is checked against the buffer,
//! nesting is bounded, and a malformed message is an error, never a panic.

#[cfg(target_os = "linux")]
pub(crate) mod bus;

use jstream_platform::PlatformError;

/// The deepest nesting of containers a received message may have. The specification
/// allows 64; the portal's replies nest four deep.
const MAX_DEPTH: usize = 32;

/// The largest message accepted, as the specification's limit (128 MiB) is far more
/// than the portal ever sends.
pub(crate) const MAX_MESSAGE: usize = 1 << 24;

/// A D-Bus value. Arrays carry their element signature, so an empty one still has a
/// type.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    /// `y`
    Byte(u8),
    /// `b`
    Bool(bool),
    /// `n`
    I16(i16),
    /// `q`
    U16(u16),
    /// `i`
    I32(i32),
    /// `u`
    U32(u32),
    /// `x`
    I64(i64),
    /// `t`
    U64(u64),
    /// `d`
    Double(f64),
    /// `s`
    Str(String),
    /// `o`
    Path(String),
    /// `g`
    Signature(String),
    /// `h`: an index into the message's file descriptors.
    Fd(u32),
    /// `a…`: the element signature and the elements.
    Array(String, Vec<Value>),
    /// `(…)`
    Struct(Vec<Value>),
    /// `{…}`, only as an array element.
    Entry(Box<Value>, Box<Value>),
    /// `v`
    Variant(Box<Value>),
}

impl Value {
    /// The value's signature.
    pub(crate) fn signature(&self) -> String {
        match self {
            Self::Byte(_) => "y".into(),
            Self::Bool(_) => "b".into(),
            Self::I16(_) => "n".into(),
            Self::U16(_) => "q".into(),
            Self::I32(_) => "i".into(),
            Self::U32(_) => "u".into(),
            Self::I64(_) => "x".into(),
            Self::U64(_) => "t".into(),
            Self::Double(_) => "d".into(),
            Self::Str(_) => "s".into(),
            Self::Path(_) => "o".into(),
            Self::Signature(_) => "g".into(),
            Self::Fd(_) => "h".into(),
            Self::Array(elem, _) => format!("a{elem}"),
            Self::Struct(fields) => {
                let inner: String = fields.iter().map(Self::signature).collect();
                format!("({inner})")
            }
            Self::Entry(k, v) => format!("{{{}{}}}", k.signature(), v.signature()),
            Self::Variant(_) => "v".into(),
        }
    }

    /// An `a{sv}` dictionary, the shape of every portal option and result set.
    pub(crate) fn dict(entries: Vec<(&str, Value)>) -> Self {
        Self::Array(
            "{sv}".into(),
            entries
                .into_iter()
                .map(|(k, v)| {
                    Self::Entry(
                        Box::new(Self::Str(k.into())),
                        Box::new(Self::Variant(Box::new(v))),
                    )
                })
                .collect(),
        )
    }

    /// The string inside a `s`, `o` or `g`.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) | Self::Path(s) | Self::Signature(s) => Some(s),
            _ => None,
        }
    }

    /// The number inside an unsigned or non-negative integer that fits in `u32`.
    pub(crate) fn as_u32(&self) -> Option<u32> {
        match self {
            Self::Byte(v) => Some(u32::from(*v)),
            Self::U16(v) => Some(u32::from(*v)),
            Self::U32(v) => Some(*v),
            Self::I32(v) => u32::try_from(*v).ok(),
            Self::U64(v) => u32::try_from(*v).ok(),
            Self::I64(v) => u32::try_from(*v).ok(),
            _ => None,
        }
    }

    /// The `i32` inside an `i`.
    pub(crate) fn as_i32(&self) -> Option<i32> {
        match self {
            Self::I32(v) => Some(*v),
            _ => None,
        }
    }

    /// The value inside a variant, or the value itself.
    pub(crate) fn unwrap_variant(&self) -> &Value {
        match self {
            Self::Variant(inner) => inner.unwrap_variant(),
            other => other,
        }
    }

    /// The entries of an `a{sv}`, keys first. `None` for any other value.
    pub(crate) fn as_dict(&self) -> Option<Vec<(&str, &Value)>> {
        let Self::Array(_, items) = self.unwrap_variant() else {
            return None;
        };
        items
            .iter()
            .map(|item| match item {
                Self::Entry(k, v) => Some((k.as_str()?, v.unwrap_variant())),
                _ => None,
            })
            .collect()
    }
}

/// Looks `key` up in an `a{sv}`'s entries.
pub(crate) fn lookup<'a>(dict: &[(&str, &'a Value)], key: &str) -> Option<&'a Value> {
    dict.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// A message's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A method call.
    Call = 1,
    /// A method's reply.
    Return = 2,
    /// A method's error reply.
    Error = 3,
    /// A signal.
    Signal = 4,
}

/// The `NO_REPLY_EXPECTED` header flag.
pub(crate) const NO_REPLY_EXPECTED: u8 = 0x1;

/// A D-Bus message: its header fields and body. The serial is assigned when it is
/// encoded.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Message {
    pub(crate) kind: Kind,
    pub(crate) flags: u8,
    pub(crate) serial: u32,
    pub(crate) path: Option<String>,
    pub(crate) interface: Option<String>,
    pub(crate) member: Option<String>,
    pub(crate) error_name: Option<String>,
    pub(crate) reply_serial: Option<u32>,
    pub(crate) destination: Option<String>,
    pub(crate) sender: Option<String>,
    /// File descriptors the message carries, out of band.
    pub(crate) unix_fds: u32,
    pub(crate) body: Vec<Value>,
}

impl Message {
    /// A method call of `interface.member` on `path` at `destination`.
    pub(crate) fn call(
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        body: Vec<Value>,
    ) -> Self {
        Self {
            kind: Kind::Call,
            flags: 0,
            serial: 0,
            path: Some(path.into()),
            interface: Some(interface.into()),
            member: Some(member.into()),
            error_name: None,
            reply_serial: None,
            destination: Some(destination.into()),
            sender: None,
            unix_fds: 0,
            body,
        }
    }

    /// The body's signature.
    pub(crate) fn signature(&self) -> String {
        self.body.iter().map(Value::signature).collect()
    }

    /// Whether this is the signal `interface.member`.
    pub(crate) fn is_signal(&self, interface: &str, member: &str) -> bool {
        self.kind == Kind::Signal
            && self.interface.as_deref() == Some(interface)
            && self.member.as_deref() == Some(member)
    }

    /// The message, little-endian, with `serial`.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] when a part is too long for the format.
    pub(crate) fn encode(&self, serial: u32) -> Result<Vec<u8>, PlatformError> {
        let mut body = Writer::default();
        for v in &self.body {
            body.value(v)?;
        }
        let body = body.buf;
        let mut fields = Vec::new();
        let mut field = |code: u8, v: Value| {
            fields.push(Value::Struct(vec![
                Value::Byte(code),
                Value::Variant(Box::new(v)),
            ]));
        };
        if let Some(p) = &self.path {
            field(1, Value::Path(p.clone()));
        }
        if let Some(i) = &self.interface {
            field(2, Value::Str(i.clone()));
        }
        if let Some(m) = &self.member {
            field(3, Value::Str(m.clone()));
        }
        if let Some(e) = &self.error_name {
            field(4, Value::Str(e.clone()));
        }
        if let Some(r) = self.reply_serial {
            field(5, Value::U32(r));
        }
        if let Some(d) = &self.destination {
            field(6, Value::Str(d.clone()));
        }
        if let Some(s) = &self.sender {
            field(7, Value::Str(s.clone()));
        }
        let sig = self.signature();
        if !sig.is_empty() {
            field(8, Value::Signature(sig));
        }
        if self.unix_fds > 0 {
            field(9, Value::U32(self.unix_fds));
        }
        let mut w = Writer::default();
        w.buf
            .extend_from_slice(&[b'l', self.kind as u8, self.flags, 1]);
        w.u32(len_u32(body.len())?);
        w.u32(serial);
        w.value(&Value::Array("(yv)".into(), fields))?;
        w.align(8);
        w.buf.extend_from_slice(&body);
        if w.buf.len() > MAX_MESSAGE {
            return Err(too_long("message"));
        }
        Ok(w.buf)
    }

    /// The length of the message `prefix` starts, once its first 16 bytes are there.
    ///
    /// # Errors
    /// When the prefix isn't a message's start or announces one that is too long.
    pub(crate) fn frame_len(prefix: &[u8]) -> Result<Option<usize>, PlatformError> {
        if prefix.len() < 16 {
            return Ok(None);
        }
        let big = match prefix[0] {
            b'l' => false,
            b'B' => true,
            _ => return Err(malformed("unknown byte order")),
        };
        let word = |at: usize| {
            let b = [prefix[at], prefix[at + 1], prefix[at + 2], prefix[at + 3]];
            usize::try_from(if big {
                u32::from_be_bytes(b)
            } else {
                u32::from_le_bytes(b)
            })
            .unwrap_or(usize::MAX)
        };
        let body = word(4);
        let fields = word(12);
        let header = 16usize
            .checked_add(fields)
            .map(|h| h.next_multiple_of(8))
            .ok_or_else(|| malformed("header length"))?;
        let total = header
            .checked_add(body)
            .ok_or_else(|| malformed("body length"))?;
        if total > MAX_MESSAGE {
            return Err(too_long("received message"));
        }
        Ok(Some(total))
    }

    /// Parses one whole message.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when it is malformed.
    pub(crate) fn decode(data: &[u8]) -> Result<Self, PlatformError> {
        let total = Self::frame_len(data)?.ok_or_else(|| malformed("truncated header"))?;
        if data.len() != total {
            return Err(malformed("length doesn't match the header"));
        }
        let big = data[0] == b'B';
        let kind = match data[1] {
            1 => Kind::Call,
            2 => Kind::Return,
            3 => Kind::Error,
            4 => Kind::Signal,
            _ => return Err(malformed("unknown message type")),
        };
        if data[3] != 1 {
            return Err(malformed("unknown protocol version"));
        }
        let mut r = Reader { data, pos: 4, big };
        let body_len = r.u32()?;
        let serial = r.u32()?;
        if serial == 0 {
            return Err(malformed("serial 0"));
        }
        let fields = r.value("a(yv)", 0)?;
        r.align(8)?;
        let mut msg = Self {
            kind,
            flags: data[2],
            serial,
            path: None,
            interface: None,
            member: None,
            error_name: None,
            reply_serial: None,
            destination: None,
            sender: None,
            unix_fds: 0,
            body: Vec::new(),
        };
        let mut signature = String::new();
        let Value::Array(_, fields) = fields else {
            return Err(malformed("header fields"));
        };
        for f in fields {
            let Value::Struct(parts) = f else {
                return Err(malformed("header field"));
            };
            let [Value::Byte(code), v] = parts.as_slice() else {
                return Err(malformed("header field"));
            };
            let v = v.unwrap_variant();
            let text = || v.as_str().map(str::to_owned);
            match code {
                1 => msg.path = text(),
                2 => msg.interface = text(),
                3 => msg.member = text(),
                4 => msg.error_name = text(),
                5 => msg.reply_serial = v.as_u32(),
                6 => msg.destination = text(),
                7 => msg.sender = text(),
                8 => signature = text().unwrap_or_default(),
                9 => msg.unix_fds = v.as_u32().unwrap_or(0),
                _ => {} // Unknown fields are ignored, as the specification says.
            }
        }
        let body_start = r.pos;
        if data.len() - body_start != usize::try_from(body_len).unwrap_or(usize::MAX) {
            return Err(malformed("body length"));
        }
        // Body alignment is relative to the body's start.
        let mut body = Reader {
            data: &data[body_start..],
            pos: 0,
            big,
        };
        let mut rest = signature.as_str();
        while !rest.is_empty() {
            let (one, tail) = split_type(rest)?;
            msg.body.push(body.value(one, 0)?);
            rest = tail;
        }
        if body.pos != body.data.len() {
            return Err(malformed("bytes after the body"));
        }
        Ok(msg)
    }
}

fn malformed(what: &str) -> PlatformError {
    PlatformError::Os {
        call: "D-Bus",
        code: None,
        message: format!("malformed message: {what}"),
    }
}

fn too_long(what: &'static str) -> PlatformError {
    PlatformError::InvalidState {
        what,
        state: "too long for D-Bus",
    }
}

fn len_u32(len: usize) -> Result<u32, PlatformError> {
    u32::try_from(len).map_err(|_| too_long("value"))
}

/// The alignment of a type by its signature's first character.
fn alignment(code: u8) -> usize {
    match code {
        b'n' | b'q' => 2,
        b'b' | b'i' | b'u' | b's' | b'o' | b'h' | b'a' => 4,
        b'x' | b't' | b'd' | b'(' | b'{' => 8,
        _ => 1, // y, g, v
    }
}

/// Splits the first complete type off a signature.
fn split_type(sig: &str) -> Result<(&str, &str), PlatformError> {
    let bytes = sig.as_bytes();
    let end = type_end(bytes, 0, 0)?;
    Ok(sig.split_at(end))
}

fn type_end(sig: &[u8], at: usize, depth: usize) -> Result<usize, PlatformError> {
    if depth > MAX_DEPTH {
        return Err(malformed("signature nests too deep"));
    }
    match sig.get(at) {
        Some(
            b'y' | b'b' | b'n' | b'q' | b'i' | b'u' | b'x' | b't' | b'd' | b's' | b'o' | b'g'
            | b'h' | b'v',
        ) => Ok(at + 1),
        Some(b'a') => type_end(sig, at + 1, depth + 1),
        Some(b'(') => {
            let mut i = at + 1;
            if sig.get(i) == Some(&b')') {
                return Err(malformed("empty struct"));
            }
            while sig.get(i) != Some(&b')') {
                if i >= sig.len() {
                    return Err(malformed("unclosed struct"));
                }
                i = type_end(sig, i, depth + 1)?;
            }
            Ok(i + 1)
        }
        Some(b'{') => {
            let key = type_end(sig, at + 1, depth + 1)?;
            let value = type_end(sig, key, depth + 1)?;
            if sig.get(value) != Some(&b'}') || key != at + 2 {
                return Err(malformed("dict entry"));
            }
            Ok(value + 1)
        }
        _ => Err(malformed("signature")),
    }
}

/// Marshals values, little-endian.
#[derive(Default)]
struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn align(&mut self, n: usize) {
        let to = self.buf.len().next_multiple_of(n);
        self.buf.resize(to, 0);
    }

    fn u32(&mut self, v: u32) {
        self.align(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn string(&mut self, s: &str) -> Result<(), PlatformError> {
        if s.contains('\0') {
            return Err(PlatformError::InvalidState {
                what: "D-Bus string",
                state: "contains NUL",
            });
        }
        self.u32(len_u32(s.len())?);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
        Ok(())
    }

    fn value(&mut self, v: &Value) -> Result<(), PlatformError> {
        match v {
            Value::Byte(b) => self.buf.push(*b),
            Value::Bool(b) => self.u32(u32::from(*b)),
            Value::I16(n) => {
                self.align(2);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::U16(n) => {
                self.align(2);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::I32(n) => {
                self.align(4);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::U32(n) | Value::Fd(n) => self.u32(*n),
            Value::I64(n) => {
                self.align(8);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::U64(n) => {
                self.align(8);
                self.buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::Double(d) => {
                self.align(8);
                self.buf.extend_from_slice(&d.to_le_bytes());
            }
            Value::Str(s) | Value::Path(s) => self.string(s)?,
            Value::Signature(s) => {
                let len = u8::try_from(s.len()).map_err(|_| too_long("signature"))?;
                self.buf.push(len);
                self.buf.extend_from_slice(s.as_bytes());
                self.buf.push(0);
            }
            Value::Array(elem, items) => {
                self.align(4);
                let at = self.buf.len();
                self.buf.extend_from_slice(&[0; 4]);
                self.align(alignment(elem.as_bytes().first().copied().unwrap_or(b'y')));
                let start = self.buf.len();
                for item in items {
                    if item.signature() != *elem {
                        return Err(PlatformError::InvalidState {
                            what: "D-Bus array",
                            state: "elements of mixed types",
                        });
                    }
                    self.value(item)?;
                }
                let len = len_u32(self.buf.len() - start)?;
                self.buf[at..at + 4].copy_from_slice(&len.to_le_bytes());
            }
            Value::Struct(fields) => {
                self.align(8);
                for f in fields {
                    self.value(f)?;
                }
            }
            Value::Entry(k, val) => {
                self.align(8);
                self.value(k)?;
                self.value(val)?;
            }
            Value::Variant(inner) => {
                self.value(&Value::Signature(inner.signature()))?;
                self.value(inner)?;
            }
        }
        Ok(())
    }
}

/// Unmarshals values by signature, in either byte order. Alignment is relative to the
/// start of `data`.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    big: bool,
}

impl Reader<'_> {
    fn align(&mut self, n: usize) -> Result<(), PlatformError> {
        let to = self.pos.next_multiple_of(n);
        let pad = self
            .data
            .get(self.pos..to)
            .ok_or_else(|| malformed("truncated"))?;
        if pad.iter().any(|&b| b != 0) {
            return Err(malformed("non-zero padding"));
        }
        self.pos = to;
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&[u8], PlatformError> {
        let end = self.pos.checked_add(n).ok_or_else(|| malformed("length"))?;
        let s = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| malformed("truncated"))?;
        self.pos = end;
        Ok(s)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], PlatformError> {
        self.align(N)?;
        let s = self.take(N)?;
        let mut b = [0; N];
        b.copy_from_slice(s);
        if self.big {
            b.reverse();
        }
        Ok(b) // little-endian from here on
    }

    fn u32(&mut self) -> Result<u32, PlatformError> {
        Ok(u32::from_le_bytes(self.fixed()?))
    }

    fn text(&mut self, len: usize) -> Result<String, PlatformError> {
        let s = self.take(len)?.to_vec();
        if self.take(1)? != [0] {
            return Err(malformed("string not NUL-terminated"));
        }
        String::from_utf8(s).map_err(|_| malformed("string not UTF-8"))
    }

    fn value(&mut self, sig: &str, depth: usize) -> Result<Value, PlatformError> {
        if depth > MAX_DEPTH {
            return Err(malformed("nests too deep"));
        }
        let bytes = sig.as_bytes();
        Ok(match bytes.first() {
            Some(b'y') => Value::Byte(self.take(1)?[0]),
            Some(b'b') => match self.u32()? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err(malformed("boolean")),
            },
            Some(b'n') => Value::I16(i16::from_le_bytes(self.fixed()?)),
            Some(b'q') => Value::U16(u16::from_le_bytes(self.fixed()?)),
            Some(b'i') => Value::I32(i32::from_le_bytes(self.fixed()?)),
            Some(b'u') => Value::U32(self.u32()?),
            Some(b'h') => Value::Fd(self.u32()?),
            Some(b'x') => Value::I64(i64::from_le_bytes(self.fixed()?)),
            Some(b't') => Value::U64(u64::from_le_bytes(self.fixed()?)),
            Some(b'd') => Value::Double(f64::from_le_bytes(self.fixed()?)),
            Some(b's') => {
                let len = self.u32()? as usize;
                Value::Str(self.text(len)?)
            }
            Some(b'o') => {
                let len = self.u32()? as usize;
                Value::Path(self.text(len)?)
            }
            Some(b'g') => {
                let len = usize::from(self.take(1)?[0]);
                Value::Signature(self.text(len)?)
            }
            Some(b'v') => {
                let len = usize::from(self.take(1)?[0]);
                let inner = self.text(len)?;
                let (one, rest) = split_type(&inner)?;
                if !rest.is_empty() {
                    return Err(malformed("variant holds more than one type"));
                }
                Value::Variant(Box::new(self.value(one, depth + 1)?))
            }
            Some(b'a') => {
                let elem = &sig[1..];
                let len = self.u32()? as usize;
                self.align(alignment(elem.as_bytes().first().copied().unwrap_or(b'y')))?;
                let end = self
                    .pos
                    .checked_add(len)
                    .ok_or_else(|| malformed("array"))?;
                if end > self.data.len() {
                    return Err(malformed("array overruns the message"));
                }
                let mut items = Vec::new();
                while self.pos < end {
                    items.push(self.value(elem, depth + 1)?);
                }
                if self.pos != end {
                    return Err(malformed("array length"));
                }
                Value::Array(elem.into(), items)
            }
            Some(b'(') => {
                self.align(8)?;
                let mut rest = &sig[1..sig.len() - 1];
                let mut fields = Vec::new();
                while !rest.is_empty() {
                    let (one, tail) = split_type(rest)?;
                    fields.push(self.value(one, depth + 1)?);
                    rest = tail;
                }
                Value::Struct(fields)
            }
            Some(b'{') => {
                self.align(8)?;
                let inner = &sig[1..sig.len() - 1];
                let (k, v) = split_type(inner)?;
                let k = self.value(k, depth + 1)?;
                let v = self.value(v, depth + 1)?;
                Value::Entry(Box::new(k), Box::new(v))
            }
            _ => return Err(malformed("signature")),
        })
    }
}

/// The client's first line of `EXTERNAL` authentication, for user `uid`: a NUL byte,
/// then `AUTH EXTERNAL` with the uid's decimal digits in hex.
pub(crate) fn auth_external(uid: u32) -> Vec<u8> {
    let hex: String = uid
        .to_string()
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut line = vec![0];
    line.extend_from_slice(format!("AUTH EXTERNAL {hex}\r\n").as_bytes());
    line
}

/// The match rule for signal `interface.member`.
pub(crate) fn match_rule(interface: &str, member: &str) -> String {
    format!("type='signal',interface='{interface}',member='{member}'")
}

/// The bus's `AddMatch` call for `rule`, so matching signals are delivered.
pub(crate) fn add_match(rule: &str) -> Message {
    Message::call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "AddMatch",
        vec![Value::Str(rule.into())],
    )
}

/// A D-Bus address's socket: `unix:path=…` or `unix:abstract=…`, the first usable
/// entry of a `;`-separated list. Escapes (`%xx`) are decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SocketAddr {
    /// A socket file.
    Path(String),
    /// A socket in the abstract namespace.
    Abstract(String),
}

/// Parses `DBUS_SESSION_BUS_ADDRESS`. `None` when no entry is a Unix socket this
/// client can reach.
pub(crate) fn parse_address(address: &str) -> Option<SocketAddr> {
    address.split(';').find_map(|entry| {
        let rest = entry.strip_prefix("unix:")?;
        rest.split(',').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            let v = unescape(v)?;
            match k {
                "path" => Some(SocketAddr::Path(v)),
                "abstract" => Some(SocketAddr::Abstract(v)),
                _ => None,
            }
        })
    })
}

fn unescape(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = char::from(bytes.next()?).to_digit(16)?;
            let lo = char::from(bytes.next()?).to_digit(16)?;
            out.push(u8::try_from(hi * 16 + lo).ok()?);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::{
        Kind, Message, SocketAddr, Value, add_match, auth_external, lookup, match_rule,
        parse_address, split_type,
    };

    fn round_trip(msg: &Message) -> Message {
        let bytes = msg.encode(7).expect("encodes");
        assert_eq!(
            Message::frame_len(&bytes).expect("a header"),
            Some(bytes.len())
        );
        Message::decode(&bytes).expect("decodes")
    }

    #[test]
    fn a_method_call_round_trips() {
        let call = Message::call(
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.ScreenCast",
            "SelectSources",
            vec![
                Value::Path("/org/freedesktop/portal/desktop/session/1_42/s1".into()),
                Value::dict(vec![
                    ("handle_token", Value::Str("jstream_2".into())),
                    ("types", Value::U32(1)),
                    ("multiple", Value::Bool(false)),
                    ("persist_mode", Value::U32(2)),
                ]),
            ],
        );
        assert_eq!(call.signature(), "oa{sv}");
        let back = round_trip(&call);
        assert_eq!(back.serial, 7);
        assert_eq!(back, Message { serial: 7, ..call });
    }

    #[test]
    fn every_basic_type_round_trips() {
        let body = vec![
            Value::Byte(9),
            Value::I16(-2),
            Value::U16(3),
            Value::I32(-4),
            Value::U32(5),
            Value::I64(-6),
            Value::U64(7),
            Value::Double(1.5),
            Value::Signature("a(ii)".into()),
            Value::Fd(0),
            Value::Array("y".into(), vec![]),
            Value::Array("t".into(), vec![]),
            Value::Struct(vec![Value::Byte(1), Value::U64(2)]),
            Value::Variant(Box::new(Value::Array(
                "(ua{sv})".into(),
                vec![Value::Struct(vec![
                    Value::U32(57),
                    Value::dict(vec![(
                        "size",
                        Value::Struct(vec![Value::I32(1920), Value::I32(1080)]),
                    )]),
                ])],
            ))),
        ];
        let mut msg = Message::call("d", "/p", "i.f", "m", body);
        msg.unix_fds = 1;
        let back = round_trip(&msg);
        assert_eq!(back.body, msg.body);
        assert_eq!(back.unix_fds, 1);
    }

    /// A reply as the bus daemon sends it, byte for byte: the `Hello` reply carrying the
    /// unique name, big-endian, to cover the other byte order.
    #[test]
    fn a_big_endian_reply_decodes() {
        let mut m = vec![b'B', 2, 0, 1];
        m.extend_from_slice(&10u32.to_be_bytes()); // body length
        m.extend_from_slice(&3u32.to_be_bytes()); // serial
        m.extend_from_slice(&15u32.to_be_bytes()); // header fields length
        // REPLY_SERIAL (5), a variant of `u`: 1.
        m.extend_from_slice(&[5, 1, b'u', 0]);
        m.extend_from_slice(&1u32.to_be_bytes());
        // SIGNATURE (8), a variant of `g`: "s".
        m.extend_from_slice(&[8, 1, b'g', 0, 1, b's', 0]);
        m.push(0); // pad the header to 8
        m.extend_from_slice(&5u32.to_be_bytes());
        m.extend_from_slice(b":1.42\0");
        assert_eq!(Message::frame_len(&m).expect("a header"), Some(m.len()));
        let msg = Message::decode(&m).expect("decodes");
        assert_eq!(msg.kind, Kind::Return);
        assert_eq!(msg.serial, 3);
        assert_eq!(msg.reply_serial, Some(1));
        assert_eq!(msg.body, vec![Value::Str(":1.42".into())]);
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        let good = Message::call("d", "/p", "i.f", "m", vec![Value::Str("hi".into())])
            .encode(1)
            .expect("encodes");
        // Every truncation, and every single-byte corruption, either decodes or errs.
        for n in 0..good.len() {
            assert!(Message::decode(&good[..n]).is_err());
        }
        for i in 0..good.len() {
            for b in [0u8, 1, 0x7f, 0xff] {
                let mut bad = good.clone();
                bad[i] = b;
                let _ = Message::decode(&bad);
            }
        }
        assert!(Message::frame_len(b"Xxxxxxxxxxxxxxxx").is_err());
        let mut huge = good.clone();
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Message::frame_len(&huge).is_err());
        let mut serial0 = good;
        serial0[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert!(Message::decode(&serial0).is_err());
    }

    #[test]
    fn signatures_split_into_complete_types() {
        assert_eq!(split_type("oa{sv}").expect("ok"), ("o", "a{sv}"));
        assert_eq!(split_type("a(ua{sv})s").expect("ok"), ("a(ua{sv})", "s"));
        assert_eq!(split_type("{sv}").expect("ok"), ("{sv}", ""));
        for bad in ["", "(", "()", "a", "{s}", "{asv}", "(i", "z"] {
            assert!(split_type(bad).is_err(), "{bad:?}");
        }
        assert!(split_type(&format!("{}y", "a".repeat(40))).is_err());
    }

    #[test]
    fn dictionaries_read_back_by_key() {
        let d = Value::Variant(Box::new(Value::dict(vec![
            ("restore_token", Value::Str("abc".into())),
            ("version", Value::U32(5)),
        ])));
        let entries = d.as_dict().expect("a dict");
        assert_eq!(
            lookup(&entries, "restore_token").and_then(Value::as_str),
            Some("abc")
        );
        assert_eq!(lookup(&entries, "version").and_then(Value::as_u32), Some(5));
        assert!(lookup(&entries, "missing").is_none());
        assert!(Value::U32(1).as_dict().is_none());
    }

    #[test]
    fn mixed_arrays_and_nul_strings_are_refused() {
        let mixed = Value::Array("u".into(), vec![Value::U32(1), Value::Str("x".into())]);
        assert!(
            Message::call("d", "/p", "i.f", "m", vec![mixed])
                .encode(1)
                .is_err()
        );
        let nul = Value::Str("a\0b".into());
        assert!(
            Message::call("d", "/p", "i.f", "m", vec![nul])
                .encode(1)
                .is_err()
        );
    }

    #[test]
    fn auth_and_match_lines() {
        assert_eq!(auth_external(1000), b"\0AUTH EXTERNAL 31303030\r\n");
        assert_eq!(
            match_rule("org.freedesktop.portal.Request", "Response"),
            "type='signal',interface='org.freedesktop.portal.Request',member='Response'"
        );
        let add = add_match("type='signal'");
        assert_eq!(add.destination.as_deref(), Some("org.freedesktop.DBus"));
        assert_eq!(add.member.as_deref(), Some("AddMatch"));
        assert_eq!(add.body, vec![Value::Str("type='signal'".into())]);
    }

    #[test]
    fn bus_addresses_parse() {
        assert_eq!(
            parse_address("unix:path=/run/user/1000/bus"),
            Some(SocketAddr::Path("/run/user/1000/bus".into()))
        );
        assert_eq!(
            parse_address("tcp:host=x,port=1;unix:abstract=/tmp/dbus-%41b,guid=0"),
            Some(SocketAddr::Abstract("/tmp/dbus-Ab".into()))
        );
        assert_eq!(parse_address("unix:path=/a%2"), None);
        assert_eq!(parse_address("tcp:host=localhost"), None);
        assert_eq!(parse_address(""), None);
    }
}
