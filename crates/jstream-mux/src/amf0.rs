//! AMF0 (Action Message Format 0), the value encoding of FLV script tags and of RTMP
//! command and data messages.
//!
//! Only the types FLV metadata and RTMP commands use are supported: number, boolean,
//! string (short and long), object, null, undefined, ECMA array and strict array. The
//! decoder is here so that the RTMP client can read server replies with the same types;
//! it treats its input as untrusted and limits nesting depth.

use crate::MuxError;

const NUMBER: u8 = 0x00;
const BOOLEAN: u8 = 0x01;
const STRING: u8 = 0x02;
const OBJECT: u8 = 0x03;
const NULL: u8 = 0x05;
const UNDEFINED: u8 = 0x06;
const ECMA_ARRAY: u8 = 0x08;
const OBJECT_END: u8 = 0x09;
const STRICT_ARRAY: u8 = 0x0A;
const LONG_STRING: u8 = 0x0C;

/// How deeply [`Amf0Value::decode`] lets objects and arrays nest.
pub const MAX_DEPTH: usize = 64;

/// One AMF0 value.
///
/// Objects and ECMA arrays keep their properties in order, because RTMP peers and FLV
/// readers expect `onMetaData` and `connect` properties in the order they were written.
///
/// ```
/// use jstream_mux::amf0::Amf0Value;
///
/// let v = Amf0Value::String("onMetaData".into());
/// let mut bytes = Vec::new();
/// v.encode(&mut bytes)?;
/// assert_eq!(&bytes[..3], &[0x02, 0x00, 0x0A]);
/// assert_eq!(Amf0Value::decode_all(&bytes)?, vec![v]);
/// # Ok::<(), jstream_mux::MuxError>(())
/// ```
#[derive(Clone, Debug, PartialEq)]
pub enum Amf0Value {
    /// An IEEE 754 double (marker `0x00`).
    Number(f64),
    /// A boolean (marker `0x01`).
    Boolean(bool),
    /// A UTF-8 string. Encoded as a string (`0x02`) up to 65535 bytes, else as a long
    /// string (`0x0C`); both decode to this variant.
    String(String),
    /// An anonymous object: ordered key/value properties (marker `0x03`).
    Object(Vec<(String, Amf0Value)>),
    /// Null (marker `0x05`).
    Null,
    /// Undefined (marker `0x06`).
    Undefined,
    /// An associative array: ordered key/value properties with a count (marker `0x08`).
    EcmaArray(Vec<(String, Amf0Value)>),
    /// A dense array of values (marker `0x0A`).
    StrictArray(Vec<Amf0Value>),
}

impl Amf0Value {
    /// Appends the encoded value to `out`.
    ///
    /// # Errors
    /// [`MuxError::TooLong`] when a property name exceeds 65535 bytes or an array has more
    /// than `u32::MAX` entries. `out` may hold part of the value then.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), MuxError> {
        match self {
            Self::Number(n) => {
                out.push(NUMBER);
                out.extend_from_slice(&n.to_be_bytes());
            }
            Self::Boolean(b) => {
                out.push(BOOLEAN);
                out.push(u8::from(*b));
            }
            Self::String(s) => match u16::try_from(s.len()) {
                Ok(len) => {
                    out.push(STRING);
                    out.extend_from_slice(&len.to_be_bytes());
                    out.extend_from_slice(s.as_bytes());
                }
                Err(_) => {
                    out.push(LONG_STRING);
                    out.extend_from_slice(&u32_len("AMF0 long string", s.len())?.to_be_bytes());
                    out.extend_from_slice(s.as_bytes());
                }
            },
            Self::Object(props) => {
                out.push(OBJECT);
                encode_properties(props, out)?;
            }
            Self::Null => out.push(NULL),
            Self::Undefined => out.push(UNDEFINED),
            Self::EcmaArray(props) => {
                out.push(ECMA_ARRAY);
                out.extend_from_slice(&u32_len("AMF0 ECMA array", props.len())?.to_be_bytes());
                encode_properties(props, out)?;
            }
            Self::StrictArray(items) => {
                out.push(STRICT_ARRAY);
                out.extend_from_slice(&u32_len("AMF0 strict array", items.len())?.to_be_bytes());
                for item in items {
                    item.encode(out)?;
                }
            }
        }
        Ok(())
    }

    /// Encodes `values` one after another, as in an RTMP command message.
    ///
    /// # Errors
    /// As [`Amf0Value::encode`].
    pub fn encode_all(values: &[Amf0Value]) -> Result<Vec<u8>, MuxError> {
        let mut out = Vec::new();
        for v in values {
            v.encode(&mut out)?;
        }
        Ok(out)
    }

    /// Decodes one value from the front of `input` and advances it past the value.
    ///
    /// # Errors
    /// [`MuxError::Amf0Truncated`], [`MuxError::Amf0UnsupportedMarker`],
    /// [`MuxError::Amf0InvalidUtf8`] or [`MuxError::Amf0TooDeep`].
    pub fn decode(input: &mut &[u8]) -> Result<Self, MuxError> {
        let mut r = Reader {
            data: input,
            pos: 0,
        };
        let v = r.value(0)?;
        *input = &input[r.pos..];
        Ok(v)
    }

    /// Decodes every value in `input`, which must end exactly after the last one.
    ///
    /// # Errors
    /// As [`Amf0Value::decode`].
    pub fn decode_all(mut input: &[u8]) -> Result<Vec<Self>, MuxError> {
        let mut values = Vec::new();
        while !input.is_empty() {
            values.push(Self::decode(&mut input)?);
        }
        Ok(values)
    }

    /// The value of property `key` when this is an object or ECMA array.
    pub fn property(&self, key: &str) -> Option<&Amf0Value> {
        match self {
            Self::Object(props) | Self::EcmaArray(props) => {
                props.iter().find(|(k, _)| k == key).map(|(_, v)| v)
            }
            _ => None,
        }
    }
}

fn u32_len(what: &'static str, len: usize) -> Result<u32, MuxError> {
    u32::try_from(len).map_err(|_| MuxError::TooLong {
        what,
        len,
        max: u32::MAX as usize,
    })
}

fn encode_properties(props: &[(String, Amf0Value)], out: &mut Vec<u8>) -> Result<(), MuxError> {
    for (key, value) in props {
        let len = u16::try_from(key.len()).map_err(|_| MuxError::TooLong {
            what: "AMF0 property name",
            len: key.len(),
            max: usize::from(u16::MAX),
        })?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(key.as_bytes());
        value.encode(out)?;
    }
    out.extend_from_slice(&[0, 0, OBJECT_END]);
    Ok(())
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], MuxError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.data.len())
            .ok_or(MuxError::Amf0Truncated {
                offset: self.data.len(),
            })?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, MuxError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], MuxError> {
        let mut a = [0; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    fn string(&mut self, len: usize) -> Result<String, MuxError> {
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| MuxError::Amf0InvalidUtf8)
    }

    fn short_string(&mut self) -> Result<String, MuxError> {
        let len = u16::from_be_bytes(self.array()?);
        self.string(usize::from(len))
    }

    fn value(&mut self, depth: usize) -> Result<Amf0Value, MuxError> {
        if depth > MAX_DEPTH {
            return Err(MuxError::Amf0TooDeep { max: MAX_DEPTH });
        }
        let marker = self.u8()?;
        Ok(match marker {
            NUMBER => Amf0Value::Number(f64::from_be_bytes(self.array()?)),
            BOOLEAN => Amf0Value::Boolean(self.u8()? != 0),
            STRING => Amf0Value::String(self.short_string()?),
            LONG_STRING => {
                let len = u32::from_be_bytes(self.array()?);
                let len = usize::try_from(len).map_err(|_| MuxError::Amf0Truncated {
                    offset: self.data.len(),
                })?;
                Amf0Value::String(self.string(len)?)
            }
            OBJECT => Amf0Value::Object(self.properties(depth)?),
            NULL => Amf0Value::Null,
            UNDEFINED => Amf0Value::Undefined,
            ECMA_ARRAY => {
                // The count is only a hint; the end marker terminates the properties.
                self.array::<4>()?;
                Amf0Value::EcmaArray(self.properties(depth)?)
            }
            STRICT_ARRAY => {
                let count = u32::from_be_bytes(self.array()?);
                let mut items = Vec::new();
                for _ in 0..count {
                    items.push(self.value(depth + 1)?);
                }
                Amf0Value::StrictArray(items)
            }
            other => return Err(MuxError::Amf0UnsupportedMarker { marker: other }),
        })
    }

    fn properties(&mut self, depth: usize) -> Result<Vec<(String, Amf0Value)>, MuxError> {
        let mut props = Vec::new();
        loop {
            let key = self.short_string()?;
            if key.is_empty() && self.data.get(self.pos) == Some(&OBJECT_END) {
                self.pos += 1;
                return Ok(props);
            }
            let value = self.value(depth + 1)?;
            props.push((key, value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Amf0Value, MAX_DEPTH};
    use crate::MuxError;

    fn enc(v: &Amf0Value) -> Vec<u8> {
        let mut out = Vec::new();
        v.encode(&mut out).unwrap();
        out
    }

    fn s(v: &str) -> Amf0Value {
        Amf0Value::String(v.into())
    }

    #[test]
    fn number_is_big_endian_double() {
        // 1.0 = 0x3FF0000000000000
        assert_eq!(
            enc(&Amf0Value::Number(1.0)),
            [0x00, 0x3F, 0xF0, 0, 0, 0, 0, 0, 0]
        );
        // 1280.0 = 0x4094000000000000
        assert_eq!(
            enc(&Amf0Value::Number(1280.0)),
            [0x00, 0x40, 0x94, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn scalars() {
        assert_eq!(enc(&Amf0Value::Boolean(true)), [0x01, 0x01]);
        assert_eq!(enc(&Amf0Value::Boolean(false)), [0x01, 0x00]);
        assert_eq!(enc(&Amf0Value::Null), [0x05]);
        assert_eq!(enc(&Amf0Value::Undefined), [0x06]);
        assert_eq!(enc(&s("ab")), [0x02, 0x00, 0x02, b'a', b'b']);
        assert_eq!(enc(&s("")), [0x02, 0x00, 0x00]);
    }

    #[test]
    fn long_string_above_65535_bytes() {
        let long = "x".repeat(65_536);
        let bytes = enc(&s(&long));
        assert_eq!(&bytes[..5], &[0x0C, 0x00, 0x01, 0x00, 0x00]);
        assert_eq!(bytes.len(), 5 + 65_536);
        assert_eq!(Amf0Value::decode_all(&bytes).unwrap(), vec![s(&long)]);
        // exactly 65535 still fits a short string
        let max = "y".repeat(65_535);
        assert_eq!(&enc(&s(&max))[..3], &[0x02, 0xFF, 0xFF]);
    }

    #[test]
    fn object_and_ecma_array() {
        let props = vec![("a".to_string(), Amf0Value::Boolean(true))];
        assert_eq!(
            enc(&Amf0Value::Object(props.clone())),
            [0x03, 0x00, 0x01, b'a', 0x01, 0x01, 0x00, 0x00, 0x09]
        );
        assert_eq!(
            enc(&Amf0Value::EcmaArray(props)),
            [
                0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, b'a', 0x01, 0x01, 0x00, 0x00, 0x09
            ]
        );
    }

    #[test]
    fn strict_array() {
        let v = Amf0Value::StrictArray(vec![Amf0Value::Null, Amf0Value::Boolean(false)]);
        assert_eq!(enc(&v), [0x0A, 0, 0, 0, 2, 0x05, 0x01, 0x00]);
    }

    #[test]
    fn rtmp_connect_round_trip() {
        let values = vec![
            s("connect"),
            Amf0Value::Number(1.0),
            Amf0Value::Object(vec![
                ("app".into(), s("live2")),
                ("tcUrl".into(), s("rtmps://a.rtmp.youtube.com/live2")),
                ("fpad".into(), Amf0Value::Boolean(false)),
                ("audioCodecs".into(), Amf0Value::Number(3575.0)),
                (
                    "nested".into(),
                    Amf0Value::EcmaArray(vec![("n".into(), Amf0Value::StrictArray(vec![]))]),
                ),
            ]),
            Amf0Value::Null,
        ];
        let bytes = Amf0Value::encode_all(&values).unwrap();
        let back = Amf0Value::decode_all(&bytes).unwrap();
        assert_eq!(back, values);
        assert_eq!(back[2].property("app"), Some(&s("live2")));
        assert_eq!(back[2].property("missing"), None);
        assert_eq!(back[0].property("app"), None);
    }

    #[test]
    fn decode_advances_input() {
        let bytes = [0x05, 0x01, 0x01];
        let mut input = &bytes[..];
        assert_eq!(Amf0Value::decode(&mut input).unwrap(), Amf0Value::Null);
        assert_eq!(input, &[0x01, 0x01]);
    }

    #[test]
    fn decode_errors() {
        assert!(matches!(
            Amf0Value::decode_all(&[0x00, 0x3F]),
            Err(MuxError::Amf0Truncated { .. })
        ));
        assert!(matches!(
            Amf0Value::decode_all(&[0x0B]),
            Err(MuxError::Amf0UnsupportedMarker { marker: 0x0B })
        ));
        assert!(matches!(
            Amf0Value::decode_all(&[0x02, 0x00, 0x01, 0xFF]),
            Err(MuxError::Amf0InvalidUtf8)
        ));
        // object without its end marker
        assert!(matches!(
            Amf0Value::decode_all(&[0x03, 0x00, 0x01, b'a', 0x05]),
            Err(MuxError::Amf0Truncated { .. })
        ));
        // a strict array that claims more items than exist must not allocate them up front
        assert!(matches!(
            Amf0Value::decode_all(&[0x0A, 0xFF, 0xFF, 0xFF, 0xFF]),
            Err(MuxError::Amf0Truncated { .. })
        ));
    }

    #[test]
    fn nesting_is_limited() {
        let mut bytes = [0x0A, 0, 0, 0, 1].repeat(MAX_DEPTH + 2);
        bytes.push(0x05);
        assert!(matches!(
            Amf0Value::decode_all(&bytes),
            Err(MuxError::Amf0TooDeep { .. })
        ));
        let mut ok = [0x0A, 0, 0, 0, 1].repeat(MAX_DEPTH);
        ok.push(0x05);
        assert!(Amf0Value::decode_all(&ok).is_ok());
    }

    #[test]
    fn property_name_too_long_is_an_error() {
        let v = Amf0Value::Object(vec![("k".repeat(65_536), Amf0Value::Null)]);
        assert!(matches!(
            v.encode(&mut Vec::new()),
            Err(MuxError::TooLong { .. })
        ));
    }
}
