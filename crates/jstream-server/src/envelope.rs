//! Reading only the envelope of a message the server won't handle: its `type` and `id`,
//! so an error about it (`malformed`, `too_large`, or MCP's JSON-RPC errors) can answer
//! the client's ID (ADR-0074).
//!
//! Nothing else in the message is built: every other member, and an `id` or `type` of
//! the wrong shape, is skipped with `IgnoredAny`. An `id` over [`MAX_ECHOED_ID`] bytes is
//! not kept either, so what an answer holds while it waits unsent stays small however
//! large the refused message was. Only a JSON object is an envelope: an array is not.

use serde::de::{self, Deserializer, IgnoredAny, MapAccess, Visitor};
use std::fmt;

/// The longest `id` an error echoes, in bytes. A longer one is answered as if it had no
/// readable `id`: the client can't have meant to match an answer by it.
pub(crate) const MAX_ECHOED_ID: usize = 256;

/// A readable `id`: a string (the control protocol's, or JSON-RPC's) or a number
/// (JSON-RPC's).
#[derive(Debug, PartialEq)]
pub(crate) enum Id {
    String(String),
    Number(serde_json::Number),
}

/// The envelope of a JSON object: whether its `type` is `request`, and its `id`.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Envelope {
    pub(crate) is_request: bool,
    pub(crate) id: Option<Id>,
}

impl Envelope {
    /// The envelope of `text`, or `None` if it isn't a JSON object.
    pub(crate) fn read(text: &[u8]) -> Option<Envelope> {
        let mut deserializer = serde_json::Deserializer::from_slice(text);
        deserializer.deserialize_map(EnvelopeVisitor).ok()
    }
}

struct EnvelopeVisitor;

impl<'de> Visitor<'de> for EnvelopeVisitor {
    type Value = Envelope;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Envelope, A::Error> {
        let mut envelope = Envelope::default();
        while let Some(key) = map.next_key::<Key>()? {
            match key {
                Key::Type => envelope.is_request = map.next_value::<IsRequest>()?.0,
                Key::Id => envelope.id = map.next_value::<ShortId>()?.0,
                Key::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(envelope)
    }
}

/// A member's name: only `type` and `id` matter. Read without allocating.
enum Key {
    Type,
    Id,
    Other,
}

impl<'de> de::Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;
        impl Visitor<'_> for KeyVisitor {
            type Value = Key;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a member name")
            }
            fn visit_str<E: de::Error>(self, key: &str) -> Result<Key, E> {
                Ok(match key {
                    "type" => Key::Type,
                    "id" => Key::Id,
                    _ => Key::Other,
                })
            }
        }
        deserializer.deserialize_identifier(KeyVisitor)
    }
}

/// Whether a `type` value is the string `request`; anything else is skipped.
struct IsRequest(bool);

impl<'de> de::Deserialize<'de> for IsRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(Skipping(|value: Scalar<'_>| {
            IsRequest(matches!(value, Scalar::Str("request")))
        }))
    }
}

/// An `id` value kept only when it is a string of at most [`MAX_ECHOED_ID`] bytes or a
/// number; anything else is skipped.
struct ShortId(Option<Id>);

impl<'de> de::Deserialize<'de> for ShortId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(Skipping(|value: Scalar<'_>| {
            ShortId(match value {
                Scalar::Str(id) if id.len() <= MAX_ECHOED_ID => Some(Id::String(id.into())),
                Scalar::Number(number) => Some(Id::Number(number)),
                _ => None,
            })
        }))
    }
}

/// A scalar value as [`Skipping`] sees it.
enum Scalar<'a> {
    Str(&'a str),
    Number(serde_json::Number),
    Other,
}

/// A visitor that hands a scalar to its function and skips arrays and objects, building
/// nothing from them.
struct Skipping<F>(F);

impl<'de, T, F: FnOnce(Scalar<'_>) -> T> Visitor<'de> for Skipping<F> {
    type Value = T;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<T, E> {
        Ok((self.0)(Scalar::Str(value)))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<T, E> {
        Ok((self.0)(Scalar::Number(value.into())))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<T, E> {
        Ok((self.0)(Scalar::Number(value.into())))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<T, E> {
        Ok((self.0)(match serde_json::Number::from_f64(value) {
            Some(number) => Scalar::Number(number),
            None => Scalar::Other,
        }))
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<T, E> {
        Ok((self.0)(Scalar::Other))
    }

    fn visit_unit<E: de::Error>(self) -> Result<T, E> {
        Ok((self.0)(Scalar::Other))
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<T, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok((self.0)(Scalar::Other))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<T, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok((self.0)(Scalar::Other))
    }
}

#[cfg(test)]
mod tests {
    use super::{Envelope, Id, MAX_ECHOED_ID};

    fn read(text: &str) -> Option<Envelope> {
        Envelope::read(text.as_bytes())
    }

    #[test]
    fn reads_type_and_a_short_id() {
        let envelope = read(r#"{"type":"request","id":"c7","call":{"kind":"nope"}}"#);
        assert_eq!(
            envelope,
            Some(Envelope {
                is_request: true,
                id: Some(Id::String("c7".into())),
            })
        );
        let rpc = read(r#"{"jsonrpc":"2.0","id":7,"method":"tools/call"}"#);
        assert_eq!(rpc.and_then(|e| e.id), Some(Id::Number(7.into())));
    }

    /// ADR-0074 review B1: an echoed `id` is bounded, so a refused message can't leave a
    /// large answer behind.
    #[test]
    fn an_id_over_the_cap_is_not_kept() {
        let at = "i".repeat(MAX_ECHOED_ID);
        let over = "i".repeat(MAX_ECHOED_ID + 1);
        let id =
            |id: &str| read(&format!(r#"{{"type":"request","id":"{id}"}}"#)).and_then(|e| e.id);
        assert_eq!(id(&at), Some(Id::String(at.clone())));
        assert_eq!(id(&over), None);
    }

    /// An `id` or `type` of another shape is skipped, and an array is no envelope (#981
    /// review N1, N2).
    #[test]
    fn other_shapes_are_skipped() {
        assert_eq!(
            read(r#"{"type":["request"],"id":[0,0,{"a":1}]}"#),
            Some(Envelope::default())
        );
        assert_eq!(read(r#"["request","c7"]"#), None);
        assert_eq!(read(r#"[5]"#), None);
        assert_eq!(read("not json"), None);
        assert_eq!(
            read(r#"{"type":"hello","id":null}"#),
            Some(Envelope::default())
        );
    }
}
