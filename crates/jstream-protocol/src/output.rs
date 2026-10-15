//! Output configuration: what an output is and where it sends, as the collection stores
//! it (docs/ARCHITECTURE.md#data-model), and the stream key, which it never stores.
//!
//! An output's kind and target are collection state, changed by edits (`output.create`,
//! `output.set_target`, `output.delete`). Its stream key is a secret (R-SEC-001,
//! docs/RULES.md#R-SEC-001): it lives only in the OS credential store, so it is set by
//! an action (`output.set_key`) and never appears in the collection, in any result,
//! event or error, or in a `Debug` print. A read reports only whether a key is set.

use crate::id::OutputId;
use schemars::JsonSchema;
use serde::de::{self, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// What an output is and where it sends. Serialized with an internal `type` tag.
///
/// Only RTMP(S) to a generic URL is configurable so far. Recording, the virtual camera
/// and the YouTube-bound target of the data model come later as new variants.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OutputKind {
    /// A stream sent over RTMP or RTMPS.
    Rtmp {
        /// The ingest server it publishes to.
        target: RtmpTarget,
    },
}

/// Where an RTMP output publishes. Serialized with an internal `type` tag.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RtmpTarget {
    /// Any RTMP(S) ingest server, named by its server URL.
    Generic {
        /// The server URL, `rtmp://host[:port]/app` or `rtmps://…`: what streaming
        /// services call the "server" or "ingest URL". The stream key is not part of
        /// it: set that with `output.set_key`. The engine rejects, with
        /// `invalid_argument`, another scheme, a missing host or application, a bad
        /// port, whitespace or control characters, and anything that can carry a
        /// credential: `user:password@`, a `?query` or a `#fragment`.
        url: String,
    },
}

/// One output, as `output.list` and `state.get` report it: its collection state.
/// Whether it runs, and whether its stream key is set, are in `output.status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutputInfo {
    /// The output's ID.
    pub output_id: OutputId,
    /// What it is and where it sends.
    pub kind: OutputKind,
}

/// A secret string, such as a stream key, sent by a client to the engine.
///
/// On the wire it is a plain JSON string. In Rust it never prints: its `Debug` is
/// `"***"`, it has no `Display`, and the only way to read it is [`expose`](Self::expose).
/// A value that is not a string fails to parse with a message that doesn't repeat it.
/// The engine never sends one back (R-SEC-001).
#[derive(Clone, PartialEq, Eq, JsonSchema)]
#[schemars(transparent)]
pub struct SecretString(String);

impl SecretString {
    /// Wraps `secret`.
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// The secret itself. Never log it or put it in an error.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"***\"")
    }
}

impl Serialize for SecretString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // `deserialize_any`, not `deserialize_string`: with the latter, a JSON
        // deserializer reports a wrong-typed value itself and quotes it.
        deserializer.deserialize_any(SecretVisitor)
    }
}

/// Accepts a string. Anything else is rejected without naming the value, so a secret
/// sent with the wrong type doesn't come back in a `malformed` reason.
struct SecretVisitor;

impl SecretVisitor {
    fn refuse<E: de::Error>(&self, what: &'static str) -> E {
        E::invalid_type(Unexpected::Other(what), self)
    }
}

impl<'de> Visitor<'de> for SecretVisitor {
    type Value = SecretString;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a string")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<SecretString, E> {
        Ok(SecretString(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<SecretString, E> {
        Ok(SecretString(v))
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<SecretString, E> {
        Err(self.refuse("a boolean"))
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<SecretString, E> {
        Err(self.refuse("a number"))
    }

    fn visit_i128<E: de::Error>(self, _: i128) -> Result<SecretString, E> {
        Err(self.refuse("a number"))
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<SecretString, E> {
        Err(self.refuse("a number"))
    }

    fn visit_u128<E: de::Error>(self, _: u128) -> Result<SecretString, E> {
        Err(self.refuse("a number"))
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<SecretString, E> {
        Err(self.refuse("a number"))
    }

    fn visit_char<E: de::Error>(self, v: char) -> Result<SecretString, E> {
        Ok(SecretString(v.to_string()))
    }

    fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<SecretString, E> {
        Err(self.refuse("bytes"))
    }

    fn visit_none<E: de::Error>(self) -> Result<SecretString, E> {
        Err(self.refuse("null"))
    }

    fn visit_unit<E: de::Error>(self) -> Result<SecretString, E> {
        Err(self.refuse("null"))
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, _: A) -> Result<SecretString, A::Error> {
        Err(self.refuse("an array"))
    }

    fn visit_map<A: de::MapAccess<'de>>(self, _: A) -> Result<SecretString, A::Error> {
        Err(self.refuse("an object"))
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputKind, RtmpTarget, SecretString};
    use crate::{ClientMessage, ProtocolError};
    use serde_json::json;

    #[test]
    fn kind_wire_form_is_tagged_twice() {
        let kind = OutputKind::Rtmp {
            target: RtmpTarget::Generic {
                url: "rtmp://a.rtmp.youtube.com/live2".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&kind).ok(),
            Some(json!({
                "type": "rtmp",
                "target": {"type": "generic", "url": "rtmp://a.rtmp.youtube.com/live2"}
            }))
        );
    }

    #[test]
    fn a_secret_is_a_plain_string_on_the_wire() {
        let secret: SecretString = serde_json::from_str("\"abcd-1234\"").expect("parses");
        assert_eq!(secret.expose(), "abcd-1234");
        assert_eq!(
            serde_json::to_string(&secret).ok().as_deref(),
            Some("\"abcd-1234\"")
        );
    }

    #[test]
    fn a_secret_never_prints() {
        let secret = SecretString::new("abcd-1234");
        let debug = format!("{secret:?} {:?}", Some(&secret));
        assert!(!debug.contains("abcd"), "{debug}");
        assert_eq!(format!("{secret:?}"), "\"***\"");
    }

    #[test]
    fn a_secret_of_the_wrong_type_is_not_echoed() {
        for (text, value) in [
            ("98765432123", "98765432123"),
            ("-98765432123", "98765432123"),
            ("98765.4321", "98765.4321"),
            ("true", "true"),
            ("[\"hunter2-key\"]", "hunter2"),
            ("{\"k\":\"hunter2-key\"}", "hunter2"),
        ] {
            let err = serde_json::from_str::<SecretString>(text)
                .expect_err("not a string")
                .to_string();
            assert!(!err.contains(value), "{text}: {err}");
            assert!(err.contains("expected a string"), "{text}: {err}");
        }
    }

    /// The same inside a whole request: serde buffers an internally tagged action
    /// before it reads the key, so the visitor has to refuse there too.
    #[test]
    fn a_set_key_request_with_a_wrong_typed_key_does_not_echo_it() {
        for key in ["98765432123", "true", "[\"hunter2-key\"]"] {
            let text = format!(
                r#"{{"type":"request","id":"r1","call":{{"kind":"action","action":{{"type":"output.set_key","output_id":"o","key":{key}}}}}}}"#
            );
            match ClientMessage::parse(&text) {
                Err(ProtocolError::Malformed { reason }) => {
                    assert!(!reason.contains("98765432123"), "{reason}");
                    assert!(!reason.contains("hunter2"), "{reason}");
                    assert!(!reason.contains("true"), "{reason}");
                }
                other => panic!("{key}: expected malformed, got {other:?}"),
            }
        }
    }
}
