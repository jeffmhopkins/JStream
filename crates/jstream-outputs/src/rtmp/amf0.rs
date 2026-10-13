//! A minimal AMF0 encoder and decoder for RTMP command messages.
//!
//! It covers the types that NetConnection and NetStream commands use. `jstream-mux`
//! will have its own AMF0 encoder for FLV script tags; the two should be unified once
//! both crates have landed (see crates/jstream-outputs/README.md#purpose).

use super::RtmpError;

const NUMBER: u8 = 0x00;
const BOOLEAN: u8 = 0x01;
const STRING: u8 = 0x02;
const OBJECT: u8 = 0x03;
const NULL: u8 = 0x05;
const UNDEFINED: u8 = 0x06;
const ECMA_ARRAY: u8 = 0x08;
const OBJECT_END: u8 = 0x09;
const STRICT_ARRAY: u8 = 0x0A;
const DATE: u8 = 0x0B;
const LONG_STRING: u8 = 0x0C;

/// How deeply objects and arrays may nest in decoded input, so a hostile server can't
/// exhaust the stack.
const MAX_DEPTH: usize = 32;

/// An AMF0 value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Amf0Value {
    Number(f64),
    Boolean(bool),
    String(String),
    /// An anonymous object; properties in wire order.
    Object(Vec<(String, Amf0Value)>),
    Null,
    Undefined,
    /// An associative ("ECMA") array; properties in wire order.
    EcmaArray(Vec<(String, Amf0Value)>),
    StrictArray(Vec<Amf0Value>),
    /// Milliseconds since the Unix epoch (the time-zone field is ignored).
    Date(f64),
}

impl Amf0Value {
    /// A string value.
    pub(crate) fn str(s: &str) -> Self {
        Self::String(s.to_owned())
    }

    /// The string, if this is one.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// The number, if this is one.
    pub(crate) fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// The named property, if this is an object or ECMA array that has it.
    pub(crate) fn get(&self, key: &str) -> Option<&Amf0Value> {
        match self {
            Self::Object(props) | Self::EcmaArray(props) => {
                props.iter().find(|(k, _)| k == key).map(|(_, v)| v)
            }
            _ => None,
        }
    }
}

/// Appends the encoding of `value` to `out`.
///
/// # Errors
/// [`RtmpError::Amf0`] when a property name is longer than 65535 bytes or an array has
/// more than `u32::MAX` elements.
pub(crate) fn encode(value: &Amf0Value, out: &mut Vec<u8>) -> Result<(), RtmpError> {
    match value {
        Amf0Value::Number(n) => {
            out.push(NUMBER);
            out.extend_from_slice(&n.to_be_bytes());
        }
        Amf0Value::Boolean(b) => {
            out.push(BOOLEAN);
            out.push(u8::from(*b));
        }
        Amf0Value::String(s) => match u16::try_from(s.len()) {
            Ok(len) => {
                out.push(STRING);
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            Err(_) => {
                let len =
                    u32::try_from(s.len()).map_err(|_| amf_err(out.len(), "string too long"))?;
                out.push(LONG_STRING);
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(s.as_bytes());
            }
        },
        Amf0Value::Object(props) => {
            out.push(OBJECT);
            encode_props(props, out)?;
        }
        Amf0Value::Null => out.push(NULL),
        Amf0Value::Undefined => out.push(UNDEFINED),
        Amf0Value::EcmaArray(props) => {
            let count =
                u32::try_from(props.len()).map_err(|_| amf_err(out.len(), "array too long"))?;
            out.push(ECMA_ARRAY);
            out.extend_from_slice(&count.to_be_bytes());
            encode_props(props, out)?;
        }
        Amf0Value::StrictArray(items) => {
            let count =
                u32::try_from(items.len()).map_err(|_| amf_err(out.len(), "array too long"))?;
            out.push(STRICT_ARRAY);
            out.extend_from_slice(&count.to_be_bytes());
            for item in items {
                encode(item, out)?;
            }
        }
        Amf0Value::Date(ms) => {
            out.push(DATE);
            out.extend_from_slice(&ms.to_be_bytes());
            out.extend_from_slice(&0i16.to_be_bytes());
        }
    }
    Ok(())
}

fn encode_props(props: &[(String, Amf0Value)], out: &mut Vec<u8>) -> Result<(), RtmpError> {
    for (key, value) in props {
        let len =
            u16::try_from(key.len()).map_err(|_| amf_err(out.len(), "property name too long"))?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(key.as_bytes());
        encode(value, out)?;
    }
    out.extend_from_slice(&[0, 0, OBJECT_END]);
    Ok(())
}

/// Decodes every value in `body`, in order.
///
/// # Errors
/// [`RtmpError::Amf0`] on truncated input, an unsupported type marker, invalid UTF-8 or
/// nesting deeper than 32 levels.
pub(crate) fn decode_all(body: &[u8]) -> Result<Vec<Amf0Value>, RtmpError> {
    let mut r = Reader { buf: body, pos: 0 };
    let mut values = Vec::new();
    while r.pos < body.len() {
        values.push(r.value(0)?);
    }
    Ok(values)
}

fn amf_err(offset: usize, reason: &'static str) -> RtmpError {
    RtmpError::Amf0 { offset, reason }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], RtmpError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or(amf_err(self.pos, "truncated value"))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], RtmpError> {
        let mut a = [0; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    fn u8(&mut self) -> Result<u8, RtmpError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, RtmpError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, RtmpError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn f64(&mut self) -> Result<f64, RtmpError> {
        Ok(f64::from_be_bytes(self.array()?))
    }

    fn utf8(&mut self, len: usize) -> Result<String, RtmpError> {
        let at = self.pos;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| amf_err(at, "string is not UTF-8"))
    }

    fn value(&mut self, depth: usize) -> Result<Amf0Value, RtmpError> {
        if depth > MAX_DEPTH {
            return Err(amf_err(self.pos, "values nest too deeply"));
        }
        let at = self.pos;
        Ok(match self.u8()? {
            NUMBER => Amf0Value::Number(self.f64()?),
            BOOLEAN => Amf0Value::Boolean(self.u8()? != 0),
            STRING => {
                let len = self.u16()?;
                Amf0Value::String(self.utf8(usize::from(len))?)
            }
            LONG_STRING => {
                let len = self.u32()?;
                let len = usize::try_from(len).map_err(|_| amf_err(at, "string too long"))?;
                Amf0Value::String(self.utf8(len)?)
            }
            OBJECT => Amf0Value::Object(self.props(depth)?),
            NULL => Amf0Value::Null,
            UNDEFINED => Amf0Value::Undefined,
            ECMA_ARRAY => {
                // The count is advisory; the object-end marker terminates the array.
                self.u32()?;
                Amf0Value::EcmaArray(self.props(depth)?)
            }
            STRICT_ARRAY => {
                let count = self.u32()?;
                let mut items = Vec::new();
                for _ in 0..count {
                    items.push(self.value(depth + 1)?);
                }
                Amf0Value::StrictArray(items)
            }
            DATE => {
                let ms = self.f64()?;
                self.u16()?;
                Amf0Value::Date(ms)
            }
            _ => return Err(amf_err(at, "unsupported AMF0 type marker")),
        })
    }

    fn props(&mut self, depth: usize) -> Result<Vec<(String, Amf0Value)>, RtmpError> {
        let mut props = Vec::new();
        loop {
            let len = self.u16()?;
            if len == 0 && self.buf.get(self.pos) == Some(&OBJECT_END) {
                self.pos += 1;
                return Ok(props);
            }
            let key = self.utf8(usize::from(len))?;
            let value = self.value(depth + 1)?;
            props.push((key, value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(v: &Amf0Value) -> Vec<u8> {
        let mut out = Vec::new();
        encode(v, &mut out).unwrap();
        out
    }

    #[test]
    fn encodes_known_bytes() {
        assert_eq!(enc(&Amf0Value::str("connect")), b"\x02\x00\x07connect");
        assert_eq!(
            enc(&Amf0Value::Number(1.0)),
            [0x00, 0x3F, 0xF0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(enc(&Amf0Value::Null), [0x05]);
        assert_eq!(enc(&Amf0Value::Boolean(true)), [0x01, 0x01]);
        assert_eq!(
            enc(&Amf0Value::Object(vec![("a".into(), Amf0Value::Null)])),
            [0x03, 0x00, 0x01, b'a', 0x05, 0x00, 0x00, 0x09]
        );
        assert_eq!(
            enc(&Amf0Value::EcmaArray(vec![])),
            [0x08, 0, 0, 0, 0, 0x00, 0x00, 0x09]
        );
    }

    #[test]
    fn round_trips_every_type() {
        let values = vec![
            Amf0Value::str("_result"),
            Amf0Value::Number(-2.5),
            Amf0Value::Boolean(false),
            Amf0Value::Null,
            Amf0Value::Undefined,
            Amf0Value::Object(vec![
                ("level".into(), Amf0Value::str("status")),
                ("code".into(), Amf0Value::str("NetStream.Publish.Start")),
                (
                    "nested".into(),
                    Amf0Value::Object(vec![("x".into(), Amf0Value::Number(1.0))]),
                ),
            ]),
            Amf0Value::EcmaArray(vec![
                ("width".into(), Amf0Value::Number(1920.0)),
                ("stereo".into(), Amf0Value::Boolean(true)),
            ]),
            Amf0Value::StrictArray(vec![Amf0Value::Number(1.0), Amf0Value::str("two")]),
            Amf0Value::Date(1.7e12),
            Amf0Value::String("é".repeat(40_000)), // 80 000 bytes: a long string
        ];
        let mut body = Vec::new();
        for v in &values {
            encode(v, &mut body).unwrap();
        }
        assert_eq!(decode_all(&body).unwrap(), values);
        assert_eq!(body[body.len() - 80_005], LONG_STRING);
    }

    #[test]
    fn accessors() {
        let o = Amf0Value::Object(vec![("code".into(), Amf0Value::str("c"))]);
        assert_eq!(o.get("code").and_then(Amf0Value::as_str), Some("c"));
        assert_eq!(o.get("missing"), None);
        assert_eq!(Amf0Value::Number(3.0).as_number(), Some(3.0));
        assert_eq!(Amf0Value::Null.as_str(), None);
    }

    #[test]
    fn rejects_bad_input() {
        let truncated = &enc(&Amf0Value::str("connect"))[..5];
        assert!(matches!(
            decode_all(truncated),
            Err(RtmpError::Amf0 { offset: 3, .. })
        ));
        assert!(matches!(
            decode_all(&[0x0D]),
            Err(RtmpError::Amf0 { offset: 0, .. })
        ));
        assert!(decode_all(&[0x02, 0x00, 0x01, 0xFF]).is_err());
        // An object missing its end marker.
        assert!(decode_all(&[0x03, 0x00, 0x01, b'a', 0x05]).is_err());
        // Deep nesting is refused instead of recursing without bound.
        let mut deep = Vec::new();
        for _ in 0..=MAX_DEPTH + 1 {
            deep.extend_from_slice(&[0x0A, 0, 0, 0, 1]);
        }
        deep.push(NULL);
        assert!(decode_all(&deep).is_err());
    }
}
